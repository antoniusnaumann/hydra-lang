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
  builtin (§7).

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
| symbol | `.name`, quoted (`."x-req-id"`) when not an identifier |
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

- §9.1 green threads on one OS thread; preemption at every statement boundary
  and at every loop iteration (`--step-budget`, default 1 statement per slice,
  raisable). Round-robin, so a schedule is reproducible and a test can assert
  on it.
- §9.4 `race`'s `end` releases control immediately; orphaned losers keep running
  and the program waits for them at exit.
- §4 a `parallel`/`race` block may not appear syntactically inside a cell;
  `check` reports it with the "call a function that opens it" suggestion.

## 12. Points where an `[O]` blocks only a warning, not the build

- §9.1 trail count for `parallel for` over a large list. **Chosen:** one trail
  per element, unbounded — the straightforward reading. A bounded pool changes
  observable interleaving, so it should be decided before anyone writes code
  that depends on it.
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
