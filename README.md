# hydra-lang

A reference implementation of **Hydra**, the language described in
[`spec/hydra_spec.md`](spec/hydra_spec.md).

Three tools, as the spec asks for:

| Tool | Command | Spec |
|---|---|---|
| interpreter | `hydra run FILE.hy` | §1–§10 |
| static checker | `hydra check FILE.hy` | §11 |
| formatter | `hydra fmt FILE.hy` | §12 |

Plus the editor support §13 describes: `hydra tokens FILE.hy` classifies a
file for semantic highlighting, and `hydra grammar` prints a TextMate grammar
(`--theme` for §13's colours). Both are generated from the same table, and
because classification runs on real tokens, `parallel for` is coloured as one
unit for free.

Calls may omit parentheses at statement level or on assignment right-hand
sides: `print "hello"`, `x := add 1, 2`. Nested calls keep parentheses.
`f -1` and `f [1]` call; `f - 1` subtracts and `f[1]` indexes. Bare names
remain function values; zero-argument calls use `()`.

`return` is a builtin constructing `[:return, ...values]`. Left unconsumed,
it returns those values from the current function; otherwise it stays data.
Bare `return` is shorthand for `return()`.

## Reference

[Compact documentation](docs/index.html) — language, builtins, atoms, and standard modules.
Run `direnv allow` once, then `docs` to regenerate, serve locally, and open the
reference. Stop it with Ctrl-C. The command is available inside this repository
when your shell has the direnv hook enabled.

Standard modules: `fs`, `env`, `text`, `json`, `io`, `time`, `http`, `list`, and `cli`.
Pass script arguments after `--`: `hydra run examples/fetch_url.hy -- https://example.com`.

## Building it

Rust/Cargo. JSON, date formatting, and HTTPS use `serde_json`, `chrono`, and `ureq`; Cargo fetches the locked dependencies automatically.

```bash
cargo build --release
```

```bash
cargo test
```

```bash
cargo run -- run examples/trails.hy --dump-scope
```

## Where things are

| File | What |
|---|---|
| `src/lexer.rs` | §1–§2: longest-match operators, compound keywords, colon atoms and dot lookups |
| `src/parser.rs` | §3–§4: the precedence table, and the column-wise `parallel` transposer |
| `src/value.rs` | §5: copy-on-write values, `&` references, the two equalities |
| `src/scope.rs` | §6: scopes as hash maps with parent pointers |
| `src/compile.rs` | the tree lowered to instructions |
| `src/vm.rs` | §5–§8: the evaluator, crashes, and `use` |
| `src/sched.rs` | §9: trails, cancel flags, the run queue, and the per-block mailboxes |
| `src/check.rs` | §11 |
| `src/format.rs` | §12 |
| `src/editor.rs` | §13: token classes, TextMate grammar, colours |
| `editor/` | the generated grammar and theme fragment |

## Implementation notes

- **Trails are green threads on a worker pool.** The evaluator is a stack
  machine, so a trail is a small suspendable state and spawning one costs an
  object rather than a thread. Those trails are then spread over a pool of OS
  threads — `--threads`, defaulting to the machine's parallelism — so CPU-bound
  work inside a `parallel` block runs on several cores:

  ```
  threads=1  0.14s      threads=4  0.09s
  threads=2  0.13s      threads=8  0.07s
  ```

  The pool bounds how many trails run at one instant, not how many exist:
  `parallel for` over 200 elements is 200 trails on however many workers.
  `RunResult::peak_parallelism` reports the most that ever ran at once, which is
  what `tests/parallelism.rs` asserts on.

  Suspension works at any depth, including inside a `parallel` block that a
  called function opened. This is the one place the implementation departs from
  the §10 checklist, which suggests a tree-walking evaluator: a tree-walker
  cannot suspend a trail that is several Rust stack frames deep. Scopes are
  still hash maps with parent pointers and names are still resolved
  dynamically, so nothing about the language changes.

- **Real threads are bought, not free.** §9.2 says writes to a parent binding
  are last-write-wins with no memory model. That stays true because every
  binding and every value node has its own lock, so a write lands whole and
  nothing is torn — the program cannot observe anything §9.2 does not describe.
  Locks are taken root-to-leaf and never re-entered; a `&` that would send a
  walk back to another root unwinds first, so the one way to build a cycle
  cannot deadlock.

  Scaling is real but not linear: every variable access goes through a lock and
  a scope chain, so per-instruction overhead dominates before the cores do.

- **Copy-on-write is structural.** Lists and dicts are nodes carrying a `shared`
  mark. Assignment, argument passing and insertion clone the *handle* and set
  the mark; the first write through a marked handle clones the node and repoints
  along the path. `===` compares node identity, which is exactly the
  "identity may be the COW buffer, for now" simplification of §5.1 — an
  untouched copy still reports identical to its source.

- **`&` is a path, not a pointer.** A reference is `(root variable cell, path)`,
  so it survives the path copying COW does underneath it, and only the lvalues
  §5.1 allows can produce one.

- **Blocks are tracked by the scheduler, not by the parent trail.** A trail can
  finish while its parent is being stepped on another worker, and a running task
  has been taken out of the task table — so the parent would never hear about
  it. Wake tokens close the same race from the other side, for a parent that is
  about to block just as its last child finishes.

- **`+=` is one instruction, and that is the whole point.** The compound
  assignments (`+= -= *= /= %= |= &= ^= <<= >>= >>>=`) are an addition to the
  spec, so QUESTIONS.md §20 records them for a ruling. `a += b` means what
  `a = a + b` means, except that it names the place once and that the read and
  the write happen under the same lock — the update is atomic **with respect to
  the place it names**, so two trails running `count += 1` add two. A load and a
  store would let one increment overwrite the other, which is what §9.2's
  last-write-wins allows and what `tests/parallelism.rs` asserts on from both
  sides. The statement is not atomic: the operand is read before the lock is
  taken, and `a` and `b` still race with each other as §9.2 says they do.

- **Cancellation falls out of the writing instructions.** `Declare`, `Store` and
  `Update` check the trail's flag after evaluating and before writing, which is §9.5's
  *evaluate → check → store*; and the flag is only consulted at a statement
  boundary of the trail's own body, so an in-flight call — and everything it
  invokes — runs to the end.

- **Everything runs on tokens, never on text** — the parallel row splitter, the
  formatter and `check` all consume the token stream. A string can contain
  `||`, `end` or `//`, so scanning lines as text would be wrong.

- **`check` switches itself off rather than guess.** §11's rule is to report
  only what is *guaranteed* to crash. When a `use`d module cannot be found, the
  file's bindings are unknowable, so name-resolution diagnostics are dropped and
  a warning says why.

## The standard library

Five builtins are specified in [`spec/hydra_stdlib.md`](spec/hydra_stdlib.md)
and implemented: `print(value, terminator = "\n")`, `has(container, key)`,
`get(container, key, fallback)`, `len(value)` and `push(&list, value)`.

`push`'s signature is the interesting one. Value semantics mean `push(rows, x)`
hands the callee a copy, so appending would be a silent no-op that looks like
working code — the `&list` in the signature makes leaving the `&` out a crash,
and `check` reports it statically. Three language features came from these five:
`&name` parameters, `name = default` parameters with named arguments, and
resolution by shape.

**Resolution.** Unqualified lookup is scope chain, then imports, then builtins.
A call tries each candidate in that order and takes the first that **accepts its
argument count and names**, so shadowing one shape leaves the others reachable —
a local `len := fn(a, b) …` takes two-argument calls and the builtin takes the
rest. `::len` is the qualified form and names the builtin outright; being
qualified it is also statically known, so `check` still reports a missing `&` on
`::push(rows, x)` in a file whose modules it could not resolve.

A missing `&` is *not* a rejection: it is reported against the candidate that
accepted the call, because it is a mistake to fix rather than a reason to
quietly run something else.

**Importing.** `use fs` brings a module in for **qualified calling only** —
`fs::read` — so an import can never quietly capture a name the file already
uses. `use fs as *` binds its names unqualified as well and is where every
shadowing warning lives; `use fs as filesystem` puts the qualified form under
that name instead. A qualifier works through the dot too, which is what lets
both the module and its functions keep lean names:

```hydra
use fs
text := path.fs::read(fallback = "")  // fs::read(path, fallback = "")
n := &rows.fs::push(1)                // fs::push(&rows, 1)
```

**Calling through a dot.** `x.f(…)` is two calls in one syntax, and the
receiver decides which: a field named `f` **holding something callable** is the
call, and otherwise it is `f(x, …)` with the receiver as the first argument.

```hydra
"hello".len()          // len("hello")
config.get(:port, 80)  // get(config, :port, 80)
obj.greet("eu")        // the field, when `:greet` holds a closure
```

Callable is the whole test, so adding a data field can never quietly capture a
call that used to reach a function. A bare `x.f` is still an ordinary key read.

Nothing is auto-referenced — `&` still marks shared mutable state where it is
written — but it **reaches through the dots to the receiver of the first call**,
because the dot is what passes it:

```hydra
&a.b            // &(a.b)      — a reference to the field
&a.foo()        // foo(&a)
&a.b.foo()      // foo(&(a.b))
&a.foo().bar()  // bar(foo(&a)) — the first call takes it, and only it
```

So `rows.push(x)` is the same error as `push(rows, x)`, and `&rows.push(x)` is
how it is written.

**`:reject` hands the call back.** A signature says what a function can be
given; only the body can say what it can be used for. A candidate that answers
`:reject` returns the call to resolution, which carries on down the same list —
so two functions may share a name *and* a shape. `reject(msg)` returns one
list, `[:reject, msg]`; an unconsumed list of this shape rejects the candidate.
`reject()` uses `:null` for the message:

```hydra
fn parse(text)
	return "the careful one"
end
fn parse(text)
	if len(text) > 3
		reject("this one only does short ones")
	end
	return "the quick one"
end

parse("ab")      // "the quick one"
parse("abcdef")  // "the careful one" — the quick one handed it back
```

**A result nothing consumes goes to where the statement stands.** In a
function, an ordinary one is dropped and `:reject` or `[:reject, msg]` returns
from the function unchanged — so a rejection passes through any number of
helpers with nothing written to pass it on:

```hydra
fn reject_if(cond)
	if cond
		return [:reject, "invalid value"]
	end
end

fn parse(text)
	reject_if(len(text) > 3)   // hands parse's call back when it rejects
	return "the quick one"
end
```

At the top level of a file an ordinary result is printed and a `:reject` is an
unhandled rejection, whose standard handler reports every refusal and fails.
A successful fallback stays silent; nested helper refusals are retained. Any
binding consumes a result — `_ = f()` says so explicitly — and consuming one
value of a call consumes them all. A later function that accepts everything an
earlier one accepts and has no way to reject makes it unreachable, which
`check` reports as an error rather than leaving dead code in the file.

Rejecting is cheap: holding the arguments for the fall-back is a handle, not a
copy. The copy happens on a **write** — the retained arguments are marked shared,
so writing to one splits a node the next candidate still needs — so `check` warns
where a write that reaches an argument comes before a rejection. Look first,
then write.

**A concrete arity beats a variadic.** A `*` parameter accepts everything
positional, so a variadic candidate is tried only after every candidate that
takes the call exactly — otherwise a variadic shadow would swallow the narrower
functions behind it.

## The `fs` module

The filesystem, specified in [`spec/hydra_fs.md`](spec/hydra_fs.md) and
demonstrated by [`examples/files.hy`](examples/files.hy). It is **built in**:
there is no file to find, `use fs` brings it in for qualified calling, and a
file named `fs.hy` beside the program shadows it — which `check` warns about.

```hydra
use fs

note, bytes := fs::write(fs::join(fs::temp(), "notes", "first.txt"), "one\n")
text := fs::read(note)
missing, why := fs::read("gone.txt", "(nothing)")   // :not_found

for entry in fs::list("src", match = "*:hy", recursive = :true)
	print("\(entry.fs::name()) is \(entry.fs::size()) bytes")
end
```

Three rules and nothing else to remember: **defaults absorb the ordinary
failures** (writing makes the parents it needs, making a directory that exists
is fine, removing what is not there answers `:false`), **anything left crashes**,
and **a reader opts out with a `fallback`** and then says why it had to.

That last one is two overloads rather than a sentinel — `read(path)` crashes and
`read(path, fallback)` does not — so no value is spent marking "no fallback
given" and every value is still a legal fallback. Every flag is keyword-only,
which is what keeps the two apart.

## Auto-channels

Trails hand values to their siblings with no channel declared anywhere: the
block *is* the channel set. Specified in
[`spec/hydra_channels.md`](spec/hydra_channels.md), demonstrated by
[`examples/channels.hy`](examples/channels.hy).

```hydra
parallel
	send("ready") || msg, ch := receive()
	              || print("got \(msg) from trail \(ch)")
end
```

`send(value, to*, mode = :wait)` answers `:true` or `:false`; `:wait` parks
until someone takes the value, `:detach` buffers, `:broadcast` buffers one copy
per eligible trail. `receive(from*)` answers with the value **and** the trail
that sent it, and both come back `:null` when no eligible sender is left — the
one answer a sender cannot fake, because a real send always arrives with a real
index behind it. `channel()` is a trail's own index.

The three are lexically scoped: `check` rejects them outside a `parallel` or
`race` body, including inside a function a trail calls. That is the deliberate
contrast with `alive()`, which is dynamic at any depth.

Two language features came out of their signatures:

- **variadic parameters** — `name*` collects the rest of the positional
  arguments into a list, a bare `*` collects nothing, and everything after
  either can only be filled by name;
- **multiple return values** — `value, ch := receive()`, from any function, not
  just the builtins. Extras a binding does not name are dropped in silence;
  naming more than arrive is a crash. A multi-value is not a value: it lives
  only between a call and a binding site.

A block whose every live trail is parked with nothing to send is a **crash**,
not a hang, and a trail cancelled while parked wakes with the closed answer and
then runs no further statement.

Everything else the spec's examples lean on is still a placeholder, so
`examples/deploy.hy` — the spec's own reference program — parses, checks and
formats but does not run. `hydra check --extern` lets you name host-provided
globals so placeholders are not reported as undeclared, and
`hydra run --dump-scope` prints a program's toplevel bindings, which is useful
when what you want to see is state rather than output.

See [`QUESTIONS.md`](QUESTIONS.md) for every point where the spec left a hole,
what this implementation does in the meantime, and what it costs to change.
