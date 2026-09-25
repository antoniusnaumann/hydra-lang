# Hydra — cheat sheet

Interpreted, dynamically typed, green-threaded, with parallelism in the syntax.
Companion to the implementation spec.

---

## Lexical

| Item | Form | Notes |
|---|---|---|
| Comment | `// to end of line` | |
| Declaration | `x := 1` | Introduces the name; on an existing name it shadows |
| Assignment | `x = 2` | Crashes if the name does not exist |
| Namespace | `json::decode` | Breaks an import clash |
| Builtin | `::push` | The language's own namespace, past any shadow |
| Default | `fn f(a, b = 1)` | Defaults come after the parameters without one |
| Named argument | `f(1, b = 2)` | After the positional ones |
| Deep equality | `a == b` | Identity first, then structural walk |
| Identity | `a === b`, `a !== b` | By identity |
| Reference | `f(&a)`, `{ :b : &a }` | Opt out of copying |
| Interpolation | `"hi \(name)"` | Any expression; `+` joins two strings |
| Trail separator | `\|\|` | Inside `parallel` / `race` blocks only |
| Bitwise | `\| & ^ ~ << >> >>>` | 32-bit, JS semantics; `\|\|` lexes first |
| Logic | `and`, `or`, `not` | Words, so `!` and `&` stay free |
| Private | `_leading_underscore` | Never exported, never reachable via `::` |
| Label | `as name` | Names a block for channel selection |
| Indentation | Tabs | **Cosmetic.** Blocks close with `end` |
| Statement end | Newline unless syntax continues | Blank lines stop continuation; no semicolons |

**Compound keywords** contain a space and lex as one token:
`else if`, `parallel for`, `parallel while`, `race for`, `race while`.
Never split them across a line or a `||`.

---

## Values

```hydra
num    := 3.0                    // 64-bit float, 32-bit for bitwise ops
text   := "hi \(name), \(a + b)"
list   := [5, 17]
dict   := { :a : 5, :"x-id" : 17 }   // keys are symbols, quotable
symbol := :null
```

- **Atoms** (`:null`, `:false`, `:ok`) are bare tags introduced by a colon.
  A dot is always a key lookup; single newlines can continue that lookup.
- **There is no struct type** — `d.a` is sugar for `d[:a]`, so a computed key is
  just `d[k]`.
- `==` **deep-compares** (cycle-safe, via a visited-pair set); `===` compares
  by reference.
- **Truthiness:** `:null` and `:false` are falsy. Everything else is truthy —
  including `0`, `""`, `[]`.
- **Everything copies.** Assignment, parameter passing, and insertion into a
  list or dict all deep-copy (implemented copy-on-write).
- `&lvalue` passes a reference instead — `f(&a)`, `{ :b : &a }`, `b := &a`.
  Only variables, dict keys and list elements can be referenced.
- `===` currently reports COW storage, so an untouched copy still compares
  identical to its source. It is not a reliable aliasing test.
- Reading a missing key **crashes**; writing one creates it.
- `x.f(…)` is the field when `:f` holds something callable, and otherwise
  `f(x, …)` — the receiver becomes the first argument. A bare `x.f` is still a
  plain key read. `x.mod::f(…)` is exactly `mod::f(x, …)`: a field cannot be
  namespaced, so there is nothing to decide.
- `&` reaches through the dots to the receiver of the **first** call:
  `&a.b` is the field, `&a.foo()` is `foo(&a)`, `&a.b.foo()` is `foo(&(a.b))`.
  Nothing is auto-referenced — `rows.push(x)` is an error, `&rows.push(x)` is
  not.
- Lists are 0-based and `a[-1]` is the last element.
- Single newlines favor continuation: `foo\n.bar` is `foo.bar`, and `32\n-a`
  is `32 - a`. Insert a blank line to begin a separate naked expression.
- Bare atoms start with a letter or `_` and consume punctuation until whitespace,
  `()[]{},:"`, `//`, or `||`: `:some-other-prop+interesting_added_info` is one
  atom. Lookup names remain identifiers: `d.total-1` subtracts; use
  `d[:x-req-id]` or `d."x-req-id"` for a key containing punctuation.
- `:"not a name"` is a symbol that isn't a valid symbol name; `:"\(x)-id"`
  builds one from data.

---

## Functions

```hydra
fn warm(name, img)
	h := lease(name)
	if not healthy(h)
		return :failed
	end
	return h
end

single := fn(a, b) a + b        // single-expression closure, no end

multi := fn(c)                  // body starts on the next line
	x := c * c
	return x - c
end
```

Closures capture by reference.

A name can mean several functions — `:=` shadows, imports stack, builtins sit
under both. A call tries them innermost-first and takes **the first that accepts
the argument count and names**; only when none does is it a crash. A missing `&`
is reported against the one that accepted, never resolved around.

---

## Process control

`exit(code)` returns `[:exit, code]`; `panic(msg)` returns `[:panic, msg]`.
Consumed lists stay data. Unconsumed lists stop the program: exit returns the
requested status (integer 0–255, default 0), while panic reports its message
and call trace on stderr with status 1. Neither retries overloads.

## Control flow

`break()` and `continue()` return ordinary `:break` / `:continue` atoms.
Left unconsumed, they affect the nearest loop in the current function.
Helpers must explicitly return them to let a caller's loop handle them.
There are no labeled breaks. In parallel loops, continue ends one iteration
and break stops the whole loop.

```hydra
if cond
	...
else if other
	...
else
	...
end

for elem in list
	continue()
	break()
end

while cond
	...
end
```

---

## Errors

No exceptions, no catch. Failure is a value, by convention a symbol:

```hydra
h := lease(name)
if h == :failed
	rollback()
end
```

A crash kills the program — unless it happens in a dead trail, where it is
isolated to that trail (reported on stderr, fatal under strict mode).

---

A statement whose result nothing consumes hands it to where it stands:

```hydra
fn reject_if(cond)
	if cond
		return [:reject, "invalid value"]
	end
end

fn foo(x)
	reject_if(x < 0)    // unconsumed `:reject`: foo returns it
	ignored := 42       // consumed: nothing happens
	42                  // unconsumed, ordinary: dropped
	return x
end

_ = foo(-1)             // `_` consumes it
foo(3)                  // top level: prints 3
foo(-1)                 // top level: crash — unhandled rejection
```

In a function an ordinary result is dropped and a `:reject` returns from it; at
the top level an ordinary result is printed (except a lone `:null`) and a
`:reject` crashes. `if`/`for`/`while` bodies are statement level too. A trail
cannot return, so a `:reject` there crashes.

A candidate that answers `:reject` hands its call back to resolution, which
tries the next candidate — so two functions may share a name *and* a shape.
`reject(msg)` returns one list, `[:reject, msg]`; `reject()` uses `:null` as
the message. An unconsumed list of this shape also rejects the candidate. When
every candidate rejects, the call answers with the rejection, and if that goes
unhandled the standard handler prints every refusal on stderr and fails.
Successful fallback prints no refusal messages. Rejecting is cheap — holding the
arguments is a handle, not a copy — but a *write* before one splits a copy that
is then thrown away, which `check` warns about. A later function that accepts
everything an earlier one does and has no way to reject makes it unreachable,
which `check` reports.

---

## Modules

```hydra
use json                // qualified only: `json::decode`
use json as *           // and unqualified: `decode`
use json as codec       // qualified, under `codec::` only

json::decode(body)
body.json::decode()     // the same call, through the dot
```

Each form gives exactly one way in, so importing a module can never quietly
capture a name — only `as *` binds bare names, and every shadowing warning lives
there.

`use` looks for a file — same directory, then `HYDRA_PATH` — and then for a
**built-in module** such as `fs`. A file of that name shadows the builtin, which
is discouraged. A qualified name resolves by shape among *that module's* own
candidates: `fs::read(p)` and `fs::read(p, "")` are two functions.

`use` **executes** a file's toplevel once, but **rebinds** it every time — so
unqualified lookup always matches source order, even for transitive imports.

Unqualified lookup is scope chain, then imports, then builtins. `::push` names
the builtin whatever else has taken it.

A qualified call resolves among *that module's* candidates and falls through to
nothing else, so a module can offer `read(path)` and `read(path, fallback)`
under one name.

`fs` is a **built-in module** (`spec/hydra_fs.md`): no file to find, and a file
named `fs.hy` shadows it, which `check` warns about.

---

## Concurrency

Trails are **green threads** sharing the parent **scope**, run on a pool of OS
threads, so CPU-bound work in a `parallel` block really runs in parallel. Writes
to a parent binding are **last-write-wins** and land whole; nothing is
guaranteed atomic across statements. Data itself is copied per trail — shared
mutable state exists only where a `&` put it.

```hydra
parallel
	eu = warm("eu", img) || us = warm("us", img) || ap = warm("ap", img)
	smoke(eu)            || smoke(us)            || smoke(ap)
end
```

- Each column is a **trail**. **Rows are cosmetic** — no barrier between them.
- `parallel` joins at `end`; `race` ends at the first completion, and stops
  spawning once it is decided.
- Every row carries the **same number of separators**; empty cells stay empty
  but keep their `||`. The block's `end` is the line with no separators.
- A trail reads and writes the parent scope, but `:=` inside a trail is
  **trail-local** and gone at the join.
- `continue()` ends a parallel iteration; `break()` stops its loop. `return` inside
  a trail is **forbidden**.
- A trail may hold full blocks (`if`, `for`) spanning rows in its column.
  A nested `parallel` must go inside a called function.

```hydra
parallel for r in REGIONS      race for r in REGIONS
	warm(r, img)                   probe(r)
end                            end
```

### Cancellation

1. A cancelled trail is **never interrupted** — its in-flight call runs to the
   end, and so does everything that call invokes.
2. The **result is discarded**; the pending assignment never happens.
3. No further statement of that trail runs.

Cancellation is **value-level, not effect-level** — a loser cannot write to the
scope, but it can still finish charging the card. It **propagates**: everything
inside a dead trail is dead. Trails are **not guaranteed to start**, so a racing
trail may never run at all.

```hydra
fn charge(card)
	tok := authorize(card)
	if not alive()
		void(tok)              // undo what the runtime cannot
		return :cancelled
	end
	return capture(tok)
end
```

`alive()` is dynamic — any function can ask, at any depth, with no token
threading, and it returns `:true` outside a trail.

---

## Tooling

- **Builtins** — `print(value, end := "\n")`, `has(c, k)`, `get(c, k, fallback)`,
  `len(v)`, `push(&list, v)`. A `&` parameter makes the call mark it or crash.
- **Formatter** — owns column padding; canonical form; idempotent.
- **`check`** — reports only what is *guaranteed* to crash. Best catch: reading
  a trail-local `:=` after the block.
- **Strict mode** — makes isolated dead-trail crashes fatal under test.

---

## Conventions

- The stdlib is not the implementor's to invent. `alive()` is the only language
  primitive; everything else in these examples is a placeholder.

- Declare shared variables **above** a parallel block, assign with `=` inside it.
- Record a race winner yourself, as the trail's last statement.
- `ALL_CAPS` for toplevel values assigned once.
