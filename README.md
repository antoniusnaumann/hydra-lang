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

## Building it

Rust, no dependencies at all — everything the spec asks for is in `std`.

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
| `src/lexer.rs` | §1–§2: longest-match operators, compound keywords, symbol-versus-lookup |
| `src/parser.rs` | §3–§4: the precedence table, and the column-wise `parallel` transposer |
| `src/value.rs` | §5: copy-on-write values, `&` references, the two equalities |
| `src/scope.rs` | §6: scopes as hash maps with parent pointers |
| `src/compile.rs` | the tree lowered to instructions |
| `src/vm.rs` | §5–§8: the evaluator, crashes, and `use` |
| `src/sched.rs` | §9: trails, cancel flags, the run queue |
| `src/check.rs` | §11 |
| `src/format.rs` | §12 |
| `src/editor.rs` | §13: token classes, TextMate grammar, colours |
| `editor/` | the generated grammar and theme fragment |

## Implementation notes

- **Trails are real green threads.** The evaluator is a stack machine, so a
  trail is a small suspendable state rather than an OS thread, and the scheduler
  multiplexes every trail onto one thread with a run queue and a step budget —
  the shape §9.1 recommends. Suspension works at any depth, including inside a
  `parallel` block that a called function opened.

  This is the one place the implementation departs from the §10 checklist, which
  suggests a tree-walking evaluator: a tree-walker cannot suspend a trail that
  is several Rust stack frames deep without either an OS thread per trail or a
  hand-rolled stack. Scopes are still hash maps with parent pointers and names
  are still resolved dynamically, so nothing about the language changes.

- **Copy-on-write is structural.** Lists and dicts are nodes carrying a `shared`
  mark. Assignment, argument passing and insertion clone the *handle* and set
  the mark; the first write through a marked handle clones the node and repoints
  along the path. `===` compares node identity, which is exactly the
  "identity may be the COW buffer, for now" simplification of §5.1 — an
  untouched copy still reports identical to its source.

- **`&` is a path, not a pointer.** A reference is `(root variable cell, path)`,
  so it survives the path copying COW does underneath it, and only the lvalues
  §5.1 allows can produce one.

- **Cancellation falls out of two instructions.** `Declare` and `Store` check
  the trail's flag after evaluating and before writing, which is §9.5's
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

Everything else the spec's examples lean on is still a placeholder, so
`examples/deploy.hy` — the spec's own reference program — parses, checks and
formats but does not run. `hydra check --extern` lets you name host-provided
globals so placeholders are not reported as undeclared, and
`hydra run --dump-scope` prints a program's toplevel bindings, which is useful
when what you want to see is state rather than output.

See [`QUESTIONS.md`](QUESTIONS.md) for every point where the spec left a hole,
what this implementation does in the meantime, and what it costs to change.
