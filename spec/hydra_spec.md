# Hydra — implementation handoff

Everything needed to build the three tools: **interpreter**, **`check`**, **formatter**.

**The implementor does not design the language.** Do not invent builtin
functions, stdlib modules, or convenience helpers — not even obvious ones like
`has`, `len`, or `print`. Where this document names such a function it is a
placeholder awaiting a separate stdlib specification from the language owner.
Only `alive()` (§9.5) is a language primitive. If something appears to be
missing, raise it as an open question rather than filling the gap.

Each rule is tagged:

- **[D]** decided — implement as written
- **[P]** proposed — a gap filled with a recommendation; cheap to change, but pick one before coding
- **[O]** open — must be decided by the language owner before the affected component is finished

---

## 1. Character level

**[D]** Source is UTF-8. Newlines terminate statements; there are no semicolons.
Indentation is tabs and is **purely cosmetic** — the parser ignores leading
whitespace entirely. Blocks are closed by `end`.

**[D]** `//` begins a comment that runs to end of line.

**[D]** No block comments. No line-continuation character; a statement is one line.

**[D]** String literals are double-quoted. **Interpolation is `\(expr)`** —
the expression is lexed and parsed recursively, so it may contain parentheses,
calls, and further strings with their own interpolations. Escapes:
`\" \\ \n \t \r \0 \(`.

**[D]** `+` also **concatenates two strings**, and does not convert: a string
and a non-string is a bad operand. Interpolation is how a value is rendered,
so `"n = " + count` is an error and `"n = \(count)"` is the way to write it.

Consequence for every tool: a string literal is no longer an opaque run of
characters. Anything that scans a line (the parallel-row splitter, the
formatter, `check`'s name resolution) must operate on **tokens**, not text.

---

## 2. Tokens

### Operators and punctuation

```
:=   =   ==   !=   ===   !==   <   >   <=   >=
+    -   *    /    %
|    &   ^    ~   <<   >>   >>>      // bitwise, 32-bit
||                                   // trail separator — NOT logical or
::                                   // namespace selector
( ) [ ] { } , . :
```

`:` separates key from value in a dict literal.

**[D]** Longest-match wins, and the order matters: `===` before `==` before `=`,
`!==` before `!=`, `::` and `:=` before `:`, `||` before `|`, `>>>` before `>>`
before `>`.

**[D]** There is no `!` operator and no `&&`. Logic is `and`, `or`, `not`.

### Keywords

```
fn  use  if  else  for  in  while  return  break  continue  end
and  or  not  parallel  race  as
```

**[D] Compound keywords** are single keywords that contain a space:

```
else if      parallel for    parallel while    race for    race while
```

They lex as one token. Internal whitespace is one or more spaces or tabs, but
**never a newline** — `else` at end of line followed by `if` on the next line is
an `else` block containing a nested `if` statement, which needs its own `end`.
The two forms behave identically and differ only in `end` count.

**[D]** A compound keyword may not be split across a `||` cell boundary.

### Identifiers

**[D]** `[A-Za-z_][A-Za-z0-9_]*`. A **leading underscore marks an item private**:
it is never exported by `use` and never reachable through `::`.

**[D]** `ALL_CAPS` is convention only and has no semantics.

### Literals

```hydra
42        3.0                        // numbers
"text"    "hi \(name), \(a + b)"     // string, with interpolation
[1, 2, 3]                            // list
{ .a : 5, .x-req-id : 17 }           // dict — keys are symbols
.null  .false  .true  .whatever      // symbols
.content-type                        // a symbol's name may contain `-`
."not a name"                        // quoted symbol
```

**[D]** A dot in leading position starts a **symbol**. A dot directly after an
expression is a **key lookup**. The lexer decides by the preceding token: after
an identifier, `)`, `]`, `}`, or a literal, `.` is a lookup; otherwise it opens
a symbol.

**[D] A symbol's name may contain `-`**, as long as it is internal:
`[A-Za-z_][A-Za-z0-9_]*(-[A-Za-z0-9_]+)*`. Nothing is lost by it, because
subtracting one symbol from another is nonsense, so `.x-req-id` can only ever
have been meant as one name. Spaces still end it: `.a - b` is a subtraction.

This holds for a symbol **literal** only, and deliberately not for a key lookup.
In `d.total-1` the thing left of the `-` is a *value*, and subtracting from it
is perfectly sensible, so the lookup form keeps reading `-` as the operator. A
hyphenated key is written `d[.x-req-id]` or `d."x-req-id"`.

**[D] Quoted symbols.** `."not a name"` is a symbol whose name is not a valid
symbol name. It works in every position a bare symbol does, including lookup:
`headers."content-type"` is `headers[."content-type"]`.

**[D]** A quoted symbol may interpolate — `."\(prefix)-id"` — which gives
dynamic symbol construction without a separate `sym(str)` builtin.

**[D] Interning.** Symbols are interned and compared by identity. Because input
data can now mint them, the intern table must be collectable (weak entries or
refcounts) or a program that parses JSON in a loop grows without bound.

**[D] Numbers are 64-bit floats.** Bitwise operators convert **in place to
32-bit**, JavaScript-style: each operand goes through ToInt32 (truncate toward
zero, then wrap modulo 2^32), the operation runs on 32-bit integers, and the
result widens back to a double.

Details that must be written into the runtime, not left to the host language:

- `NaN`, `+Inf`, `-Inf` convert to `0`.
- Values outside int32 range wrap; `2^31` becomes `-2^31`.
- Anything above 2^53 has already lost precision before the conversion sees it.

**[D] Shifts follow JS too:** `<<` and `>>` operate on int32 and are signed;
`>>>` is the unsigned right shift and yields a **uint32** result, so it is the
one operator that can produce a value above 2^31 - 1. Shift counts are taken
modulo 32.

---

## 3. Grammar

```ebnf
program    = { stmt } ;

stmt       = use | fndecl | decl | assign | if | for | while
           | parallel | race | break | continue | return | exprstmt ;

use        = "use" ident ;
fndecl     = "fn" ident "(" [ params ] ")" NEWLINE block "end" ;
params     = param { "," param } ;
param      = [ "&" ] ident [ "=" expr ] ;
args       = arg { "," arg } ;
arg        = [ ident "=" ] expr ;

decl       = ident ":=" expr ;
dict       = "{" [ dictent { "," dictent } ] "}" ;
dictent    = symbol ":" expr ;
symbol     = "." ident | "." string ;
string     = '"' { char | "\\(" expr ")" } '"' ;
assign     = lvalue "=" expr ;
lvalue     = ident | postfix "." ident | postfix "[" expr "]" ;

if         = "if" expr NEWLINE block
             { "else if" expr NEWLINE block }
             [ "else" NEWLINE block ] "end" ;
for        = "for" ident "in" expr [ "as" ident ] NEWLINE block "end" ;
while      = "while" expr [ "as" ident ] NEWLINE block "end" ;

parallel   = ( "parallel" | "race" ) [ "as" ident ] NEWLINE rows "end"
           | ( "parallel for" | "race for" ) ident "in" expr [ "as" ident ]
             NEWLINE block "end"
           | ( "parallel while" | "race while" ) expr [ "as" ident ]
             NEWLINE block "end" ;
rows       = { row NEWLINE } ;
row        = cell { "||" cell } ;
cell       = { token } ;                  (* see §4 *)

break      = "break" [ ident | "trail" ] ;
continue   = "continue" [ ident ] ;
return     = "return" [ expr ] ;

expr       = ... (* precedence table below *) ;
closure    = "fn" "(" [ params ] ")" ( expr | NEWLINE block "end" ) ;
```

**[D]** Whether a closure is single-expression or multi-line is decided by
whether anything follows the `)` **on the same line**.

**[D] Parameters.** `&name` requires the *call* to pass a reference; see §5.1.
`name = expr` gives the parameter a default, evaluated in the **function's own
scope** at each call where the argument is missing, so a later default may refer
to an earlier parameter. Parameters with defaults come **after** those without,
and a `&` parameter may not have a default — a default is a value, and a
reference has to come from a call site.

**[D] Named arguments.** `f(a, width = 2)` fills a parameter by name. Named
arguments come after the positional ones, and no parameter may be filled twice.
There is no ambiguity with assignment: assignment is a *statement*, so it can
never appear inside an argument list.

A **keyword is not a name**, so neither a parameter nor an argument may be
called `end`, `in`, `as` or any other keyword.

**[D] Resolution.** A name can mean more than one function — `:=` shadows (§6),
imports stack (§7), and the builtins sit under both. A call tries each in that
order — innermost binding first, then the most recent `use`, then the builtin —
and takes **the first that accepts it**. A candidate *rejects* a call when it
has too many arguments, a name the candidate does not have, a parameter filled
twice, or one it needs and did not get. Only when nothing accepts is it a crash
(§8), and the diagnostic lists what was tried.

Shadowing a function with one of a different shape therefore does not hide the
original:

```hydra
fn f(a)
	return "one"
end
f := fn(a, b) "two"
f(1)         // "one" — the shadowing one rejects a single argument
f(1, 2)      // "two"
```

**[D] `reject()` hands the call back.** A signature says what a function can be
*given*; only the body can say what it can be *used for*. `reject()` leaves the
function and returns the call to resolution, which carries on down the same
list — so two functions may share a name **and** a shape:

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

parse("ab")      // "the quick one"    — the later one, as always
parse("abcdef")  // "the careful one"  — the quick one handed it back
```

**[D]** The message is optional and is what a crash prints. When nothing takes
the call, every refusal is listed with it, nearest first:

```
crash: no `parse` took 1 argument(s)
  parse(text) rejected it: this one only does short ones
  parse(text) rejected it
```

**[D]** `reject()` belongs in a **function**. At a file's toplevel or in a trail
body there is no call to hand back, and it crashes saying so; `check` reports it
before it runs.

**[D] A shadow that never rejects is an error.** Two functions of one name and
one shape are only useful because the later can hand a call back — so a later
one that accepts everything an earlier one accepts and contains no `reject()`
makes the earlier unreachable, and `check` reports that rather than leaving dead
code in the file. A **variadic** never shadows a concrete arity, since it is
tried only after every one of them.

**[D]** A **`&` mismatch is not a rejection.** It is reported against the
candidate that accepted the call, because a missing `&` is a mistake to fix,
not a reason to quietly run something else.

**[D]** A **qualified** call — `mod::f(…)` or `::f(…)` — does not fall through
to anything *outside* what it names: no other module, and not the builtins. It
still resolves by shape **within** it, because one module may export several
functions of a name — `fs::read(path)` and `fs::read(path, fallback)` are two
candidates and the call picks the one that accepts it.

### Operator precedence **[D]**

Loosest to tightest — Python's ordering, deliberately *not* C's, so that
`flags | MASK == x` groups as `(flags | MASK) == x`:

| Level | Operators | Assoc |
|---|---|---|
| 1 | `or` | left |
| 2 | `and` | left |
| 3 | `not` | prefix |
| 4 | `==` `!=` `===` `!==` `<` `>` `<=` `>=` | left |
| 5 | `\|` | left |
| 6 | `^` | left |
| 7 | `&` | left |
| 8 | `<<` `>>` `>>>` | left |
| 9 | `+` `-` | left |
| 10 | `*` `/` `%` | left |
| 11 | unary `-` `~`, reference `&` | prefix |
| 12 | call `f(…)`, index `a[i]`, key `a.b`, namespace `m::n` | postfix |

---

## 4. Parsing a parallel block

This is the only unusual part of the parser. **Parse column-wise, not
line-wise.**

**[D] Separator discipline** — every row of a block carries the *same* number of
`||` separators. Cells may be empty; separators may not be omitted, including
trailing ones. This is what makes the block's own `end` unambiguous: it is the
first line inside the block that contains **zero** separators.

> This supersedes the earlier informal note that trailing empty cells could be
> dropped. Without it, a row whose only content is a cell-internal `end` is
> indistinguishable from the block terminator.

### Algorithm

1. On `parallel` / `race`, read raw lines until a line with zero `||` whose
   trimmed text is `end`.
2. Split every row on top-level `||`. *Top-level* means not inside `(`, `[`,
   `{`, or a string literal — **including inside a `\(…)` interpolation**, which
   is why this step runs on tokens rather than on raw text.
3. Verify all rows have the same cell count → otherwise a hard parse error.
4. **Transpose**: concatenate cell *k* of every row, in row order, into trail
   *k*'s token stream. Trim each cell.
5. Parse each trail's stream as an ordinary statement list; blocks inside a
   trail must balance within that trail.
6. Keep the original `(line, column)` of every cell so diagnostics point at the
   real source position, not the transposed one. This matters more than it
   sounds — every error message in a parallel block depends on it.

### Nested blocks inside a cell

**[D]** A cell may contain full statements including `if` / `for` / `while` with
their `end`, spanning several rows in its own column.

**[P]** A `parallel` or `race` block may **not** appear syntactically inside a
cell — its rows would need their own `||`, which collides with the outer split.
Nest by calling a function that opens the inner block. `check` reports this as a
hard error with that suggestion.

---

## 5. Values

**[D]**

| Kind | Notes |
|---|---|
| number | 64-bit float; 32-bit for bitwise ops |
| string | immutable; supports `\(expr)` interpolation, and `+` concatenates two strings |
| list | mutable, **value semantics** (§5.1) |
| dict | mutable, **symbol keys only**, **value semantics** |
| symbol | interned tag, e.g. `.null` |
| closure | captures its enclosing **scope**, not a copy of it |

**[D]** There is no separate struct type — a "struct" is a dict with symbol
keys. `d.field` is sugar for looking up the symbol `.field`, so `d.a` and
`d[.a]` are the same operation, and a computed key is just `d[k]`.

### 5.1 Value semantics and `&`

**[D]** Every value behaves as a value. **Assignment, parameter passing,
insertion into a list or dict, and returning all copy deeply.** Nothing is
aliased implicitly.

```hydra
b := a          // b is an independent copy
f(a)            // f cannot touch the caller's a
d := { .x : a } // the dict holds a copy
```

**[D]** `&lvalue` passes a reference instead — the caller marks it, never the
callee:

```hydra
f(&a)
d := { .x : &a }
b := &a
list := [&a, &b]
```

**[D]** Only an **lvalue** may be referenced: a variable, a dict key, a list
element. `&(a + b)` and `&f()` are errors.

**[D] A parameter may require it.** `fn push(&list, value)` says the call must
mark the argument; `push(rows, x)` **crashes**. This does not move the marking
to the callee — the caller still writes `&`, and can still see it at the call
site — it only makes leaving it out an error instead of a silent no-op. Without
it, a mutating helper called by value appends to a *copy*, which looks exactly
like working code.

**[D] Copy-on-write is required**, not optional. The deep copy is a semantic
guarantee; implementations must make it cheap rather than literal. Expected
shape: refcounted nodes, path copying on write — mutating `d.a.b` when `d`'s
node is shared clones `d`, then clones `d.a` if that node is shared too, and
repoints along the path.

**[D] Identity may be the COW buffer, for now.** `===` compares storage, so a
copy that has not yet been written to still reports as identical to its source:

```hydra
b := a
b === a         // .true  — they still share a buffer
b.x = 1         // write splits the buffer
b === a         // .false — from here on
c := &a
c === a         // .true  — always
```

This is a deliberate simplification, accepted to keep the first implementation
small. What it costs:

- `===` is **not a reliable aliasing test**. Code cannot ask "was this handed to
  me with `&`?", because an untouched copy answers the same way.
- COW timing becomes observable, so changing the copying strategy later — say,
  copying small values eagerly — changes program results.
- Combined with the identity fast path in `==` (§5), a structure containing
  `NaN` compares equal to its untouched copy and unequal after any write to
  either. The only case where `==` is not stable under an unrelated mutation.

**[O]** Un-leaking it later means one field: an identity stamp per value handle,
minted on copy and independent of the buffer. Worth doing before `===` appears
in anyone's published code.

**[D]** Consequences for concurrency: a trail's copies are its own. Only two
things can race — bindings in the shared parent scope, and values that were
handed over with `&`. **`&` is the marker for shared mutable state**, and is
worth reading as such at every call site.

**[D] Truthiness:** `.null` and `.false` are falsy. *Everything else is truthy*,
including `0`, `""`, and `[]`.

**[D]** Reading a field that does not exist is a **crash** (see §8).

**[D] Equality:** `===` compares **by identity**; `!==` is its negation.
`==` **deep-compares** — element by element for lists, key by key for dicts,
recursively.

**[D]** Since `a === b` implies `a == b`, deep comparison **tries identity
first** and returns `.true` immediately when it holds, before any walk. This is
the fast path for the common case of comparing a value with itself.

One deliberate consequence: a structure containing `NaN` is `==` to itself,
because identity settles it before the walk ever reaches the `NaN`.

**[D]** For values with no identity of their own — numbers, strings, symbols —
`===` is value equality, so `"a" === "a"` is true. Making strings compare by
pointer would be a footgun in a language whose strings are immutable. `NaN` is
never equal to itself under either operator.

**[D]** Closures have no structural equality; `==` falls back to identity.

**[D] Cycles** are handled with a **visited-pair set**. Deep comparison keeps a
set of `(a, b)` reference pairs currently being compared; re-encountering a pair
returns `.true` rather than recursing. Two cyclic structures of the same shape
therefore compare equal, and comparison always terminates.

Fast paths worth having: identical references short-circuit to `.true` before
anything else, and the set is only allocated once recursion passes a small depth.

**[D]** List indexing (`a[i]`) is **0-based**, and a **negative index counts
from the end**, so `a[-1]` is the last element. An index still outside the list
after that crashes, consistently with a missing key.

**[D] Key writes create.** Reading a missing key crashes, but `d.k = v` and
`d[k] = v` **create** the key. Without this a dict can never be built
incrementally, and with no exceptions there is no other way to recover.

**[D] Key writes create.** Reading a missing key crashes; `d.k = v` and
`d[k] = v` create it. `d.k` is **exactly** sugar for `d[.k]` — one code path in
the evaluator, not two.

**[O] Reading a maybe-missing key.** Since reads crash and nothing can be
caught, the stdlib must provide `has(d, .k)` and probably `get(d, .k, default)`.
Any program touching decoded JSON needs them on the first line.

### 5.2 Calling through a dot

**[D]** `x.f(…)` is **two calls in one syntax**, and the receiver decides which:

1. If `x` has a field `f` **holding something callable**, that is the call, and
   the receiver is not passed — a closure in a dict is called with exactly the
   arguments written.
2. Otherwise the call is `f(x, …)`: the receiver becomes the **first
   argument**, and `f` resolves like any other name — scope chain, then
   imports, then builtins, first candidate that accepts it (§3).

```hydra
"hello".len()          // len("hello")   -> 5
config.get(.port, 80)  // get(config, .port, 80)
obj.greet("eu")        // the field, when `.greet` holds a closure
```

**[D]** *Callable* is the whole test for step 1. A field named `count` holding a
number is not what `x.count()` meant, so it falls through to the function. This
is deliberate: it means adding a data field to a dict can never quietly capture
a call that used to reach a function, unless the field holds a function too.

**[D]** Only an **unquoted, non-interpolated** key is a function name.
`d."x-y"(…)` and `d[k](…)` are ordinary field calls and crash when the field is
missing, as they always did.

**[D]** `x.mod::f(…)` is exactly `mod::f(x, …)` and skips step 1 entirely: a
field cannot be namespaced, so there is nothing to decide (§7).

**[D]** A **bare `x.f` is still an ordinary key read**, and still crashes when
the key is missing. Nothing is bound or partially applied by writing the dot
without a call.

**[D]** The receiver is passed **exactly as written**, which is what keeps §5.1
intact: `&` still marks shared mutable state at the site that writes it, and
nothing is ever auto-referenced.

**[D] A `&` reaches through a postfix chain to the receiver of the first call.**
The dot is what passes the receiver, so the dot is what the marker reaches:

```hydra
&a.b            // &(a.b)      — a reference to the field, as before
&a.foo()        // foo(&a)
&a.b.foo()      // foo(&(a.b))
&a.foo().bar()  // bar(foo(&a)) — the first call takes it, and only it
&f(x)           // an error: no receiver to mark. Write `f(&x)`.
```

This is the one place the marker is not written immediately in front of the
thing it marks, and it reads that way because that is where the receiver is.
`rows.push(x)` is still the same error as `push(rows, x)`; `&rows.push(x)` is
how it is written, and `push(&rows, x)` still says the same thing.

**[D]** A **field** call is handed no receiver, so a `&` in front of one has
nothing to mark and **crashes**. Fields still win (§5.2 step 1) — the marker is
what is wrong, not the call — and the diagnostic says so.

**[D]** `check` reports what is guaranteed: when the receiver **provably** has
no such field — a literal that is not a dict, or a dict whose keys are known —
the call is certainly the free one, so its shape and its missing `&` are checked
against the function.

---

## 6. Scope and binding

**[D]**

- `x := expr` **declares** `x` in the current scope. On a name that already
  exists it **shadows** — a fresh binding, so closures that captured the old one
  keep the old one.
- `x = expr` **assigns** to an existing binding, searching outward through the
  scope chain. If no binding exists, it **crashes**.
- A trail reads and writes its parent scope, but everything it declares with
  `:=` is **local to that trail** and gone at the join.
- Function bodies, loop bodies, `if` bodies and trails each open a scope.
- A closure captures the **scope chain**, not a copy of it. Value semantics
  govern how values move between bindings; they do not change what a closure
  can see or write.

Implementation: a scope is a hash map plus a parent pointer. A trail's scope's
parent is the block's enclosing scope. Scopes themselves are never copied — it
is the **values** flowing between bindings that copy (§5.1).

---

## 7. Modules

**[D]** `use name` does two separable things:

1. **Execute** the file's toplevel code — **skipped** if that file is already in
   scope, so side effects happen exactly once per program.
2. **Bind** the module into the importing file — this **always** runs, even on a
   repeat `use`.

Keeping (2) unconditional is what makes "most recent `use` wins" match source
order even when a module was already pulled in transitively.

**[D] Three forms, and each gives exactly one way in:**

| Written | Reaches it as | Unqualified names |
|---|---|---|
| `use fs` | `fs::read` | — |
| `use fs as *` | `fs::read` | `read` |
| `use fs as filesystem` | `filesystem::read` | — |

A plain `use` brings a module in **for qualified calling only**. Nothing of it
is reachable bare, so importing a module can never quietly capture a name the
file already uses, and a module is free to call its functions `read`, `list` and
`size` without asking what else is in the program.

`as *` is how a file says it wants the names themselves, and it is where every
shadowing diagnostic lives — that warning now marks a deliberate act rather than
an accident. The module's own name stays a qualifier alongside, because two star
imports that collide need a way to say which one is meant.

`as name` puts the qualified form under that name **instead**: after
`use fs as filesystem`, `fs::read` is not in scope. One import, one way in.

**[D]** `mod::name` selects explicitly and is the way to disambiguate.
Private (`_`-prefixed) names are not reachable through it.

**[D] A qualifier works through the dot too**, which is what makes a lean
module name and a lean function name compose:

```hydra
use fs
text := path.fs::read("")     // fs::read(path, "")
n := &box.counter::bump(1)    // counter::bump(&box, 1)
```

`x.mod::f(…)` is exactly `mod::f(x, …)`. There is nothing for the receiver to
decide (§5.2): a field cannot be namespaced, so the qualified form is always the
free call, and the `&` reaches the receiver the same way it does without a
module.

**[D] Qualified syntax wins.** `::name` — the same selector with the module
omitted — names the **language's own namespace**: the builtin, whatever else has
taken the name. Unqualified lookup goes scope chain, then imports, then
builtins, so a module that exports `push` shadows the builtin one and `::push`
is how the builtin is still reached. `check` warns at the `use` that does it,
because a silently shadowed name still returns *something*.

**[D]** `::name` is not a variable: it can be called and passed around, but
never assigned to or referenced with `&`. Neither is a builtin module's name:
`fs::read` can be called, but there is no cell behind it to pass around.

**[D] A qualified call resolves among the module's own candidates.** It falls
through to nothing — never to another module's function, nor to a builtin — but
a module may have more than one function under a name, and resolution by shape
(§3) picks between them:

```hydra
use fs
fs::read("a.txt")      // read(path)
fs::read("a.txt", "")  // read(path, fallback)
```

That is what lets a module offer a crashing reader and a falling-back one under
one name (`spec/hydra_fs.md` §1) rather than spending a value on a sentinel.

**[D]** Resolution order for `use fmt`: same directory, then a path list from an
environment variable, then the **built-in modules**. Circular imports resolve to
whatever is bound so far rather than looping forever.

**[D] Built-in modules** are namespaces of native functions that the interpreter
and `check` know without a file — `fs` is the first of them
(`spec/hydra_fs.md`). They come last in the resolution order, so a file named
`fs.hy` beside the program shadows the built-in one. That is legal and
**discouraged**, and `check` warns at the `use` that does it: a shadowed module
is the same failure mode as a shadowed name, and it still returns *something*.

---

## 8. Errors

**[D]** There are no exceptions and no catch. Failure is an ordinary value —
by convention a symbol such as `.failed`.

**[D]** A **crash** (missing field, bad operand, `=` to an undeclared name, a
call no candidate accepts, an argument a `&` parameter needs and did not get,
explicit abort) terminates the program — *unless* it happens in
a dead trail (§9.5), in which case it is isolated to that trail.

**[D]** When a **live** trail crashes: mark every sibling cancelled, let each
finish its in-flight statement, print the diagnostic, exit non-zero.

---

## 9. Concurrency

### 9.1 Threads

**[D]** Trails are **green threads**.

**[D]** Trails are multiplexed onto a **pool of OS threads**, so CPU-bound work
in a `parallel` block really does run on several cores. The pool size bounds how
many trails make progress at one instant; it does not bound how many trails
there are.

A trail is still a green thread — spawning one costs a small object, not a
thread — and a trail still suspends and resumes anywhere. What real threads cost
is the free lunch a single thread was giving: "simple arithmetic is atomic in
practice" is no longer had for nothing. It is bought instead, by locking each
binding and each value node, which is what keeps §9.2's rule true — a write to a
parent binding lands whole, so concurrent writes really are last-write-wins and
nothing is ever torn.

**[D] Scheduling points** are I/O suspension and long-running computation.
Nothing is guaranteed atomic; in practice a statement that neither performs I/O
nor runs long will not be interleaved.

**[P]** Concretely: check for preemption at statement boundaries, at any I/O
suspension, and every N interpreter steps inside a long statement. Cancellation
is checked at *every* statement boundary regardless; N only decides how often a
trail is handed back to the queue, and with a pool it wants to be large enough
that the scheduler is not the bottleneck.

**[D]** Trails are **not guaranteed to start immediately**. A trail may still be
unscheduled when a `race` is decided, in which case it never runs at all. Never
put required side effects in a racing trail.

**[D]** Trail count for `parallel for` over a large list: **one trail per
element**, unbounded. The *thread pool* is the bound that matters, and it is on
how many run at once rather than on how many exist.

### 9.2 Shared state

**[D]** Trails share the parent **scope**, so concurrent writes to a parent
binding are **last-write-wins**. No locks, no atomics, no memory model — the
race is embraced.

**[D]** Values a trail merely reads and works with are its own copies (§5.1).
Shared mutable *data* exists only where a `&` put it, which makes `&` crossing
into a trail the thing to look for when auditing a race.

### 9.3 `parallel`

**[D]** Every trail runs; control passes `end` only when all have finished.
Rows are **cosmetic** — there is no barrier between them.

### 9.4 `race`

**[D]** The block is decided when the first trail finishes. Losers are cancelled
(§9.5).

**[D]** `end` releases control immediately; the runtime keeps orphaned trails
alive to completion and waits for them at program exit. Discarding results
(§9.5) is what makes that safe for data.

**[D]** A `race` **stops spawning** once it is decided: `race while` re-checks
before evaluating its condition again, and `race for` before taking the next
element. A trail started after the decision would be born cancelled and run no
statement at all, so starting it is pure waste.

**[D]** Nothing records the winner. If you need to know, write it down as the
trail's last statement.

### 9.5 Cancellation

**[D]** A cancelled trail is **never interrupted**:

1. Its in-flight call runs to the end, together with everything that call
   invokes. No frame is ever abandoned, so acquire/release pairs always complete.
2. The **result is then discarded** — the pending assignment does not happen.
3. No further statement of that trail runs.

Implementation: *evaluate → check the cancel flag → only then store*.

**[D]** Cancellation is **value-level, not effect-level**. A losing trail cannot
write to the scope, but it can still finish sending the request or charging the
card.

**[D]** Cancellation **propagates**: everything inside a dead trail is dead,
including any `parallel` or `race` a called function opens. A dead trail cannot
spawn work that outlives it.

**[D]** `alive()` reports whether the calling trail still matters. It is a
**dynamic** property of the current trail — no token threading — and returns
`.true` outside any trail. It also goes false during crash shutdown, so one
check covers both paths.

**[D]** A crash inside a **dead** trail is **isolated**: that trail ends, the
program continues. Dead-trail crashes are reported on stderr by default
(silenceable) and are **fatal under strict mode**, so tests fail on bugs that
production would swallow.

### 9.6 Control flow inside trails

**[D]** `return` inside a trail is **forbidden** — `check` rejects it.
`break` ends the current trail early. Cancelling *siblings* is `race`'s job
alone and has no other syntax.

**[D] Labels.** A loop or block may be labelled with `as name`, and
`break name` / `continue name` target it. `break trail` ends the innermost
trail from any depth — `trail` is a reserved label, not an identifier.

---

## 10. Interpreter checklist

1. Lexer with longest-match, compound keywords, symbol-vs-access disambiguation.
2. Line-oriented reader that hands `parallel` bodies to the transposer (§4).
3. Tree-walking evaluator over scopes with parent pointers.
4. Trail objects: `{ scope, cancel_flag, state }`; `alive()` reads the flag of
   the current trail; children inherit a pointer to the parent's flag so
   propagation is a walk up, not a broadcast.
5. Store-after-check on every assignment, so cancellation discards cleanly.
6. Scheduler: run queue, I/O readiness, step budget.
7. Join logic for `parallel`; first-completion logic for `race`.
8. Crash handling split by trail liveness.
9. `use` cache keyed by resolved path, with rebinding on every `use`.

---

## 11. `check` — static analysis

**[D] Principle:** report only what is **guaranteed** to crash at runtime, never
what might. This language has enough legitimate nondeterminism that a
maybe-list would be enormous and would be ignored within a week.

**Hard errors:**

| Diagnostic | Basis |
|---|---|
| `=` to a name with no binding in any enclosing scope | scope walk |
| Read of a name declared only inside a trail, used after the block | trail-local rule §6 |
| `return` inside a trail | §9.6 |
| `break` / `continue` with an unresolvable label | label table |
| `break` / `continue` outside any loop (except `break trail` inside a trail) | §9.6 |
| Key read on a dict literal that provably lacks the key | local dataflow |
| Undeclared name used inside a `\(…)` interpolation | token-level resolution |
| Non-symbol key in a dict literal | grammar |
| `&` applied to something that is not an lvalue | §5.1 |
| Duplicate key in one dict literal | grammar |
| `_private` name reached through `::` | §7 |
| A call no statically known candidate accepts | call graph |
| An argument for a `&` parameter that is not a reference | §5.1 |
| `parallel` / `race` written syntactically inside a cell | §4 |
| Rows of a block with differing separator counts | §4 |
| Compound keyword split across lines or across `\|\|` | §2 |

**Warnings:**

- An unqualified name that two `use`d modules both export — silent shadowing is
  the failure mode that reaches production, because the wrong `decode` usually
  still returns *something*.
- `else` followed by a lone nested `if` — probably a mis-spelled `else if`.
- A required side effect inside a racing trail, where detectable.
- An unused private (`_name`).
- A `&` reference crossing into a trail — the only way to share mutable data
  between trails, and worth a second look every time.

---

## 12. Formatter

**[D]** The formatter owns column padding, and its output is the canonical form.

**Rules:**

1. One tab per nesting level; no spaces for indentation, ever.
2. One space around binary operators and after commas; none inside `(`/`[`, and
   none between `&` and its lvalue — `&a`, never `& a`.
3. Compound keywords normalise to exactly one space and are **never** wrapped.
3a. Dict literals normalise to `{ .a : 1, .b : 2 }` — spaces inside the braces
   and around the colon. Interpolations normalise like ordinary expressions:
   `\(a + b)`, never `\( a+b )`. A quoted symbol whose content is a valid
   symbol name is rewritten bare: `."name"` becomes `.name` and `."x-req-id"`
   becomes `.x-req-id`. A *key lookup* is not a symbol literal, so
   `d."x-req-id"` keeps its quotes — bare, the `-` there would subtract.
   **[D]**, one line each to change.
4. Never move a line break, since a newline terminates a statement — the
   formatter must not join or split statement lines.
5. Inside a `parallel` block:
   - split each row on top-level `||`;
   - pad every cell to the widest in its column, then join with ` || `;
   - emit a separator for every column on every row, empty cells included;
   - never wrap a cell.
6. Idempotent: formatting formatted source changes nothing.

**Known cost:** aligning to the widest cell means a one-character edit re-pads
the whole block, so a diff touches every line of it. That is the accepted trade —
the alternative (a fixed column stride) produces quieter diffs and occasional
ragged columns. **[O]** if this is ever revisited.

---

## 13. Editor support — token classes

For a TextMate grammar / LSP semantic tokens. Reference colours are the
VS Code Dark+ mapping used in the mock-ups.

| Class | Members | Colour |
|---|---|---|
| `keyword.concurrency` | `parallel`, `race`, and every compound built on them — coloured as **one unit** | `#ff8a65` semibold |
| `punctuation.trail` | `\|\|` | `#ff8a65` @ 55% |
| `keyword.control` | `if`, `else`, `else if`, `for`, `in`, `while`, `return`, `break`, `continue`, `end` | `#c586c0` |
| `keyword.other` | `fn`, `use`, `and`, `or`, `not`, `as` | `#569cd6` |
| `entity.symbol`, `entity.namespace` | `.null`, `json::` | `#4ec9b0` |
| `variable`, `variable.property` | | `#9cdcfe` |
| `entity.function` | | `#dcdcaa` |
| `constant` | `ALL_CAPS` | `#4fc1ff` |
| `string` | | `#ce9178` |
| `constant.numeric` | | `#b5cea8` |
| `comment` | | `#6a9955` italic |

---

## 14. Reference program

```hydra
// deploy.hy — warm three regions at once, then verify

use fmt
use http as *            // `push` below is http's, so this file asks for the
use json                 // bare names; `decode` is only ever qualified

REGIONS := ["eu", "us", "ap"]

fn warm(name, img)
	h := lease(name)
	push(h, img)
	wait_ready(h, 60)
	if not healthy(h)
		drain(h)
		return .failed
	end
	return h
end

manifest := json::decode(read_file("deploy.json"))
img := manifest.image        // same as manifest[.image]

for name in REGIONS
	print("target \(name) -> \(img)")
end

eu := .null
us := .null
ap := .null

parallel
	eu = warm("eu", img) || us = warm("us", img) || ap = warm("ap", img)
	smoke(eu)            || smoke(us)            || smoke(ap)
end

down := 0
for h in [eu, us, ap]
	if h == .failed or not live(h)
		down = down + 1
	end
end

if down > 0
	rollback()
end
print("\(3 - down)/3 regions live")
```

Note the trails use `=`, not `:=` — `:=` inside a trail would declare a
trail-local that vanishes at the join, which is `check`'s single most valuable
diagnostic.

`print`, `read_file`, `lease`, `smoke` and the rest are **placeholders**, not a
proposed stdlib. See the note at the top of this document.

---

## 15. Still open

1. Reading a maybe-missing key — `has` / `get`. Blocks any program that parses
   JSON. **Highest priority.** A symbol that isn't a valid identifier is
   spelled `."like-this"`, and built from data with `."\(x)"`.
2a. Whether `===` keeps reporting COW storage or gains a real identity stamp.
5. Whether `break` inside `parallel for` ends that iteration's trail (consistent
   with `break` in a plain trail) or stays invalid.
6. Every remaining **[P]**: the preemption points, and whether a `parallel`
   block may be written syntactically inside a cell.
7. The standard library. `print`, `has`, `get`, `len` and `push` are specified
   in `hydra_stdlib.md`; everything else the examples lean on is still
   undefined. `push(&list, value)` set the precedent value semantics demanded:
   a mutating helper takes `&` first and crashes without it.
