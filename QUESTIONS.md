# Open questions for the language owner

Everything here is a hole in the spec that the implementation had to step over to
keep moving. Each entry records **what was chosen**, so nothing is silently
invented, and **what it costs** if the owner rules differently.

Anything marked **BLOCKING** could not be worked around: the affected component
cannot be finished until it is decided.

---

## 1. The standard library — **PARTLY DECIDED** (spec §15.7)

The owner specified five builtins, drafted in `spec/hydra_stdlib.md` and now
implemented: `print`, `has`, `get`, `len`, `push`. A program can finally say
what it computed, and decoded data can be read without crashing.

Three language features came out of their signatures, all now in the handoff:

- `&name` parameters (§5.1) — the call must pass a reference or crash, which is
  what stops `push(rows, x)` from being a silent no-op;
- `name = default` parameters and named arguments (§3);
- resolution by shape (§3) — a call takes the first candidate that accepts its
  argument count and names, so shadowing `len` leaves the builtin reachable for
  the calls the shadow rejects.

**Still open:**

- Everything else §14 leans on: `read_file`, `lease`, `wait_ready`, `healthy`,
  `drain`, `smoke`, `live`, `rollback`, `json::decode`. `--extern` still exists
  for those.
- String helpers, any I/O, and whether there is an `eprint`.
- `print`'s second parameter is `terminator`, not `end`: `end` closes every
  block, so it can never be a name. Swift's `print(_:terminator:)` is the
  precedent, and the parser now says exactly why if you try `end`.
- ~~The name `push` is contested.~~ **Resolved:** qualified syntax wins.
  Whichever module supplies §14's `push(h, img)` shadows the builtin for
  unqualified calls, `check` warns at the `use`, and `::push` reaches the
  builtin (§7). Since a plain `use` no longer binds bare names, that file now
  says `use http as *` — and the warning marks a deliberate act rather than an
  accident.

---

## 2. List indexing — **DECIDED by the owner** (spec §15.2, §5)

**Ruling:** 0-based, and a **negative index counts from the end**, so `a[-1]` is
the last element. A non-integer index crashes; an index still outside the list
after wrapping crashes, consistent with a missing dict key. Implemented in
`resolve_index` in `src/value.rs`.

`a[i] = v` still requires an existing index and never extends a list — until
there is a `push`, lists can only be built as literals.

---

## 3. Value-to-string conversion — **DECIDED by the owner** (spec §1)

The first spec update removed interpolation on the grounds that `+` covered it.
It did not: without conversion nothing could render a number as text, and with
conversion `+` becomes silently lossy.

**Ruling:** interpolation is back, and `+` does **not** convert. `+` joins two
strings or adds two numbers; a string and a non-string is a bad operand, and the
crash says to interpolate instead. `"n = \(count)"` is how a value is rendered.

Interpolation is therefore where the text form of a value is defined:

| Value | Text |
|---|---|
| number | shortest round-tripping form; `3.0` prints as `3`, `NaN`/`Infinity`/`-Infinity` spelled out |
| string | itself, uninterpreted |
| symbol | `.name`, and `-` is part of a name (`.x-req-id`); quoted (`."not a name"`) when it is not one |
| list | `[1, 2]` |
| dict | `{ .a : 1 }` |
| closure | `fn(a, b)` |

**Cost if changed:** `to_text` in `src/value.rs`.

A list or dict renders as its literal form, which round-trips through the
parser. Nothing in the spec asks for that, but it makes `"\(d)"` useful for
debugging while there is no standard library.

---

## 3a. Building a symbol from a string — **RESOLVED** (§2)

`."\(prefix)-id"` is the spelling, and it works again now that interpolation is
back: the symbol is built and interned at run time. No `sym(str)` is needed.

Symbols minted this way are exactly why §2 requires the intern table to be
collectable; it is, by weak entries.

---

## 4. Operand types for arithmetic and comparison

The spec gives `+ - * / %` and `< > <= >=` without saying what they accept, and
"bad operand" is listed as a crash.

**Chosen:** `+` concatenates two strings and adds two numbers (see §3 above);
every operator is otherwise **numbers only** and anything else crashes. `%` follows
C/JavaScript (`fmod`, sign of the dividend), not Python's floored `%`. Division
by zero yields `Infinity`/`NaN` like every other f64 operation rather than
crashing, since the spec makes numbers IEEE doubles.

**Cost if changed:** `binary_op` in `src/value.rs`.

---

## 5. `for … in` operand

§3 gives `for ident in expr`; §5 does not say what is iterable.

**Chosen:** lists only. A dict, string, number or symbol crashes. Dict iteration
would need a decision on what the loop variable is (key, value, or a pair) and on
ordering, which is language design.

---

## 6. Assignment through a reference

§5.1 says `&lvalue` passes a reference and that only the caller marks it, but not
what `x = 5` does when `x` holds a reference.

**Chosen:** it writes **through** the reference, so `f(&a)` with `fn f(x) x = 5 end`
sets the caller's `a`. Rebinding instead would make `&` useless for numbers and
strings. `x := 5` still shadows with a fresh local binding, per §6.

Reading a reference **derefs transparently**, so `b := c` where `c` holds a
reference copies the target — you must write `&` again to keep aliasing, which is
what "the caller marks it, never the callee" implies.

---

## 7. Number literal syntax

§2 shows `42` and `3.0` only.

**Chosen:** `digits [ "." digits ] [ ("e"|"E") ["+"|"-"] digits ]`. Exponents are
included because doubles cannot otherwise be written in full range. No hex,
octal, binary or digit separators — those would be inventions.

---

## 8. Blank and comment-only lines inside a `parallel` block

§4's separator discipline says every row carries the same number of `||`. Taken
literally, a blank line is a row with zero separators, which is both a count
mismatch and a `end`-lookalike.

**Chosen:** a line with **no tokens at all** (empty, whitespace, or only a
comment) is not a row and is skipped. Any line with content must carry the
row's separator count.

---

## 9. Module search path (spec §7)

"a path list from an environment variable" — the variable is not named.

**Chosen:** `HYDRA_PATH`, `:`-separated, searched after the importing file's own
directory. Files are `NAME.hy`.

---

## 10. `use` binding scope (spec §7)

"Bind its non-private names into the global lookup" — one program-wide table, or
one per importing file?

**Chosen:** per importing file. A module's imports do not leak into its
importers, which is what makes "most recent `use` wins matches source order"
meaningful per file. Imported names bind to the *module's own storage cell*, so
`fmt::x` and an unqualified `x` are the same variable.

---

## 11. Points where a `[P]` was adopted as-is

Implemented exactly as recommended, listed so they are easy to revisit:

- §9.1 preemption at every statement boundary and at every loop iteration
  (`--step-budget`). Cancellation is checked at every boundary regardless; the
  budget only decides how often a trail returns to the queue. It defaults to 1
  on a single worker — reproducible, which is what the schedule-asserting tests
  use — and to 64 with a pool, where a shorter slice spends more time in the
  scheduler's lock than in the program.
- §9.4 `race`'s `end` releases control immediately; orphaned losers keep running
  and the program waits for them at exit.

**Superseded by the owner:** §9.1's single-OS-thread coroutine runtime. Trails
now run on a pool of OS threads so CPU-bound work is actually parallel, and
`race` stops spawning once it is decided. Both are recorded as **[D]** in the
handoff.
- §4 a `parallel`/`race` block may not appear syntactically inside a cell;
  `check` reports it with the "call a function that opens it" suggestion.

## 12. Points where an `[O]` blocks only a warning, not the build

- ~~§9.1 trail count for `parallel for` over a large list.~~ **Decided:** one
  trail per element, unbounded. The thread pool is the bound that matters, and
  it bounds how many run at once rather than how many exist.
- §9.5/§15.5 whether `break` inside `parallel for` ends that iteration's trail.
  **Chosen:** yes, it ends that iteration's trail only, consistent with `break`
  in a plain trail. `check` does not reject it.
- §5.1/§15.2a whether `===` gains a real identity stamp. **Chosen:** COW storage,
  as §5.1 says, including the documented consequence that an untouched copy
  reports identical.

---

## 13. A label on a `parallel` / `race` block (spec §9.6)

"A loop or block may be labelled with `as name`, and `break name` /
`continue name` target it." What `break name` means when `name` labels a
*block* rather than a loop is not stated, and a trail cannot cancel its
siblings — "cancelling siblings is `race`'s job alone".

**Chosen:** `break <block label>` from inside a trail ends **that trail**,
exactly like `break trail`. `continue <block label>` is rejected: there is no
next iteration of a block.

---

## 14. What `parallel while` / `race while` spawn (spec §3, §9)

The grammar has the form but no semantics beyond the block kinds.

**Chosen:** the condition is evaluated in the *parent*, and each time it is
truthy the body is spawned as one more trail; the block then joins (or decides,
for `race`) as usual. So `parallel while` is `while`, with each iteration's body
becoming a trail instead of running inline.

Left unresolved: whether a `race while` should stop spawning as soon as the race
is decided. It currently spawns while the condition holds; trails spawned after
the decision are born cancelled and so run no statement at all.

---

## 15. Rooting an assignment or a `&`

§5.1 allows `&` on "a variable, a dict key, a list element", and the §3 grammar
writes an lvalue as `postfix "." ident`, which would also admit `f().x = 1`.

**Chosen:** an assignment target and a `&` target must be rooted at a **name**
(possibly `mod::name`). `f().x = 1` is a compile error, since it could only ever
write into a temporary.

---

## 16. `break trail` is lexical (spec §9.6)

"`break trail` ends the innermost trail from any depth." Block depth and call
depth are both "depth".

**Chosen:** block depth. `break trail` is valid anywhere lexically inside a
trail body, including inside loops and `if`s, but not inside a function the
trail calls — a function does not know it is running in a trail, and making it
know would need the dynamic trail stack that `alive()` deliberately replaces.

---

## 17. Trailing comments and cells (spec §12)

§12's rules cover indentation, spacing, dict literals and column padding, but
say nothing about comments or about the inside of a cell.

**Chosen:**

- A trailing comment is separated from the code by exactly one space. The
  alignment in the spec's own §14 listing is therefore not preserved — it is
  the kind of thing rule 5 says the formatter owns.
- A cell's contents are rendered with the ordinary spacing rules but with **no
  indentation of their own**, even when a cell holds an `if` spanning three
  rows. Tabs inside a padded column would break the alignment the same rule
  demands, and the alternative — padding with spaces inside cells — makes the
  block's own indentation ambiguous.

---

## 18. How a program reports anything at all

Not a spec hole so much as the sharpest edge of §1 above, listed separately
because it affects the tools rather than the language.

`hydra run --dump-scope` prints the toplevel bindings a program ends with. It
exists so that the interpreter can be demonstrated and tested at all, and it is
a flag on the tool, not a builtin: nothing in the language can reach it.

---

## 19. What real threads cost, and what is left

Scaling is real but not linear — roughly 2x on eight workers for eight
CPU-bound trails. Every variable access takes a lock and walks a scope chain, so
the interpreter's own per-instruction overhead is reached before the cores are.

Two things were worth fixing and are done: the scope walk no longer touches the
refcount of scopes every trail shares, and arithmetic no longer formats an error
string it usually throws away.

What is left, in the order it would pay:

- **Resolve names at compile time.** Slot indices instead of a hash lookup per
  access would remove most of the remaining lock traffic. It is a real change:
  §6's scopes are hash maps by specification, and `:=` shadowing plus the
  resolution rules of §3 mean a slot is not always statically knowable.
- **Per-worker run queues.** One mutex serialises every scheduling decision.
- **Read-mostly scopes.** A scope is written once per declaration and read
  constantly; something cheaper than an `RwLock` would suit it.

None of these change the language, so none of them is blocking.

---

## 20. Compound assignment — **NEEDS A RULING** (spec §3, §9.2)

Not a hole in the spec: an **addition** to it. `+=` is nowhere in §3's grammar or
its precedence table, and the language it describes is complete without it. It
was asked for, so it is implemented, and it is recorded here because the owner
has not ruled on it.

**What was added.** One compound assignment per arithmetic and bitwise operator
of §3's table — `+= -= *= /= %= |= &= ^= <<= >>= >>>=` — and none for the
comparisons or for `and` / `or`, which answer a question rather than combining
two operands into a new value. Each lexes as **one** operator, so §2's
longest-match rule now also reads `>>>=` before `>>>` and `+=` before `+`.

**It is a statement, not an expression.** §3 says assignment is a statement, and
that is what keeps `f(a = 1)` unambiguously a named argument. `a += 1` is the
same statement with an operator attached, so `f(a += 1)` is a syntax error and
nothing new can appear inside an expression. The grammar addition is one line:

```
assign = lvalue ( "=" | compound ) expr ;
```

**What it means.** `place op= expr` means what `place = place op expr` means,
with two differences, both deliberate:

- **The place is named once.** `a[next()] += 1` calls `next` once. The path is
  evaluated once and shared by the read and the write.
- **The read and the write are one step**, which is the part that is not sugar
  (below).

Reading is still reading: `d.k += 1` on a dict with no `.k` **crashes**, where
`d.k = 1` would create the key (§5). It is the crash `d.k = d.k + 1` raises on
its way to the write, and creating the key would mean inventing an identity
element per operator.

**Atomicity.** §9.2 says concurrent writes to a parent binding are
last-write-wins, and §9.1 says nothing is guaranteed atomic. A compound
assignment is the one exception, and it is narrow: the read and the write happen
under the **same lock**, so the update is atomic *with respect to the place it
names*. Two trails running `count += 1` add two — no increment is lost. Nothing
else changed: the operand was evaluated before the lock was taken, so `a += b`
uses the `b` it read, and `a` and `b` are still last-write-wins with respect to
one another. The statement is not atomic; the update to the place is.

This is worth having precisely because §9.1 no longer gives it away for free.
With trails on real threads, `count = count + 1` is a read and a write with a gap
in between, and `tests/parallelism.rs` asserts on both halves of that: eight
trails and 4000 increments land exactly 4000 through `+=`, and never more than
4000 through the two-step form.

**Cost if the owner says no:** delete `Instr::Update` and `update_place`; the
lexer table, one parser branch and `Stmt::Assign`'s `op` field go with them.
**Cost if the owner wants it to be an expression instead:** the named-argument
disambiguation of §3 has to be decided first — `f(a += 1)` and `f(a = 1)` cannot
both be what they look like.
