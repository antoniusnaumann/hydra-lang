# hydra-lang

A reference implementation of **Hydra**, the language described in [`spec/hydra_spec.md`](spec/hydra_spec.md).

Three tools, as the spec asks for:

| Tool | Command | Spec |
|---|---|---|
| interpreter | `hydra run FILE.hy` | §1–§10 |
| static checker | `hydra check FILE.hy` | §11 |
| formatter | `hydra fmt FILE.hy` | §12 |

## Building it

Rust, no dependencies at all — everything the spec asks for is in `std`.

```bash
cargo build --release
```

```bash
cargo test
```

```bash
cargo run -- check examples/deploy.hy
```

## Implementation notes

- **Trails are real green threads.** The evaluator is a stack machine, so a
  trail is a small suspendable state rather than an OS thread, and the scheduler
  (`src/sched.rs`) multiplexes every trail onto one thread with a run queue and
  a step budget — the shape §9.1 recommends. Suspension works at any depth,
  including inside a `parallel` block opened by a called function.
  (This is the one place the implementation departs from the §10 checklist,
  which suggests a tree-walking evaluator: a tree-walker cannot suspend a trail
  that is several Rust stack frames deep without either OS threads or an unsafe
  hand-rolled stack. The semantics are unchanged.)
- **Copy-on-write is structural.** Lists and dicts are nodes carrying a `shared`
  mark. Assignment, argument passing and insertion clone the *handle* and set
  the mark; the first write through a marked handle clones the node and repoints
  along the path. `===` compares node identity, which is exactly the
  "identity may be the COW buffer, for now" simplification of §5.1.
- **`&` is a path, not a pointer.** A reference is `(root variable cell, path)`,
  so it survives the path copying COW does underneath it, and only the lvalues
  §5.1 allows can produce one.
- **Everything runs on tokens, never on text** — the parallel row splitter, the
  formatter and `check` all consume the token stream. A string can contain
  `||`, `end` or `//`, so scanning lines as text would be wrong.

## The standard library is deliberately absent

Per the note at the top of the spec, the implementor does not design the
language. `alive()` is the only primitive this implementation defines. Calling
any other undefined name crashes, exactly as it should.

That makes `examples/deploy.hy` — the spec's own reference program —
unrunnable until a stdlib is specified, so it ships as a *parse/check/format*
fixture rather than an executable one. `hydra check --extern` lets you name
externally provided globals so the checker does not report placeholders as
undeclared.

See [`QUESTIONS.md`](QUESTIONS.md) for every point where the spec left a hole,
what this implementation does in the meantime, and why.
