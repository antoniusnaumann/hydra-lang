# Hydra — standard library

**Status: implemented**, with the language owner's two corrections applied —
`print` takes a defaulted `end`, and `push` declares `&list` in its signature.
Both needed new language surface, now in the handoff: `&` parameters (§5.1) and
default parameter values (§3).

Same tags as the handoff: **[D]** decided, **[P]** proposed, **[O]** open.

---

## 1. What a builtin is

**[P]** These five are **global names**, not a module. They are in scope in
every file without a `use`, and they are *not* reachable through `::` — there is
no module for them to belong to.

**[P]** They are looked up **after the scope chain and after imports**, so a
program or a module may shadow one: `print := fn(x) … end` wins for the rest of
that scope, exactly as §6's outward walk implies. That keeps them from being
reserved words.

**[D]** `::name` reaches the builtin past any shadow (§7). It is the qualified
form, and being qualified it is always statically known — `check` can report a
missing `&` on `::push(rows, x)` even in a file where a module could not be
resolved.

**[D]** They are ordinary values. `f := print` binds it, `f("hi")` calls it.

**[D]** Arguments may be named (§3): `print(v, terminator = "")`. And because a
call takes the first candidate that accepts it, shadowing one of these with a
function of a different shape leaves the builtin reachable for the calls the
shadow rejects.

---

## 2. `print(value, terminator = "\n")` → `:null`

**[D]** Writes the **text form** of `value` — the same rendering `\(value)`
produces (§1) — followed by `end`, to standard output.

It defaults to a newline, so `print(v)` writes a line and `print(v, "")` writes
without one:

```hydra
print("no newline", terminator = "")
print(" — and now one")
```

**[D]** The parameter is `terminator`, **not `end`**: `end` closes every block
in this language (§1), so it can never be a name. Swift's `print(_:terminator:)`
is the precedent.

One *value* argument, not many: interpolation already composes, so
`print("a \(b) c")` covers what a variadic `print` would, and Hydra has no
variadic calls.

**[D]** Returns `:null`, so it is a statement, not an expression to build with.

**[P] Ordering under concurrency.** Output is written in the order trails
actually run, which is the scheduler's order and not the order of the columns.
Two trails printing interleave by line, never mid-line: one `print` is one write.

**[O]** Whether there is a matching `eprint` for stderr, and whether `print`
should flush. Both matter for a language whose crashes go to stderr.

---

## 3. `has(container, key)` → `:true` / `:false`

**[P]** For a **dict**, whether `key` — a symbol — is present.
For a **list**, whether `key` — a number — is an index the list has, with the
negative-index rule of §5 applied first, so `has(xs, -1)` is `:false` only on an
empty list.

**[D]** Crashes if `container` is neither a dict nor a list, or if the key's kind
does not match the container's. Asking about the wrong kind of thing is a bug,
not a `:false`.

This is the function §15.1 says every program touching decoded data needs on its
first line, because a missing-key *read* crashes and nothing can be caught.

---

## 4. `get(container, key, fallback)` → value

**[P]** The value at `key`, or `fallback` when it is missing. Same container and
key rules as `has`.

**[D]** `fallback` is **required** — deliberately, even though defaults now
exist. There is no obvious value to default it to (`:null` is a legitimate
thing to have stored), and requiring it makes the missing case visible at the
call site.

**[D]** The result copies, like every other read (§5.1). `get(d, :k, [])` hands
back a fresh empty list, not a shared one.

**[P]** `get` does **not** create. `d.k = v` is how a key comes into existence
(§5), and a reading function that quietly wrote would be a trap.

---

## 5. `len(value)` → number

**[P]**

| Argument | Result |
|---|---|
| list | element count |
| dict | key count |
| string | **character** count, not bytes — source is UTF-8 (§1) |

**[D]** Crashes on a number, symbol or closure. A "length" for those would be
invented, and there is nothing to count.

---

## 6. `push(&list, value)` → number

**[P]** Appends `value` to `list` and returns the list's **new length**.

**[D] `&list` is in the signature**, which is what makes `push(rows, x)` — without
the `&` — crash. The rule is general: any parameter written `&name` requires the
call to pass a reference (§5.1).

This is the rule the language's value semantics force, and it is worth being
loud about. Passing `rows` by value hands `push` a *copy* (§5.1); appending to
it would be a silent no-op that looks exactly like working code. Crashing turns
the language's most surprising interaction into an error message at the call
site:

```hydra
rows := []
push(rows, 1)      // crash: push needs a reference — write push(&rows, 1)
push(&rows, 1)     // 1
push(&rows, 2)     // 2
```

**[D]** `value` is copied in, like any insertion (§5.1). `push(&rows, &item)`
inserts a reference instead, and is how a list of shared handles is built.

**[D]** `push` splits a shared list before appending, like any other write, so
a copy taken earlier does not see the new element (§5.1).

**[P]** This makes `push` the shape every future mutating helper follows:
**a mutating helper declares `&` and the call site shows it.** Deciding that now
is what §15.7 means when it says the stdlib's signatures determine where `&`
appears in ordinary code.

---

## 7. What this unblocks, and what it does not

With these five, §14's reference program still needs `read_file`, `lease`,
`wait_ready`, `healthy`, `drain`, `smoke`, `live`, `rollback` and
`json::decode`. What it does unblock is everything a *test* needs: a program can
finally report what it computed, and decoded data can be read without crashing.

**[O]** Still absent and still needed by ordinary code: string helpers (there is
no way to split, trim, or compare case-insensitively), list helpers beyond
`push`, and any I/O at all. Each one wants the same `&`-first decision `push`
just made.

---

## 8. `check` and the formatter

**[P]** `check` gains two rules, both of which fit §11's "only what is
guaranteed":

| Diagnostic | Basis |
|---|---|
| an argument for a `&` parameter that is not a reference | the signature — a guaranteed crash |
| a call none of the candidates accepts | they are statically known functions |

The five stop being undeclared names, so `hydra check --extern print,…` is no
longer needed for them. Both rules are switched off when a `use`d module cannot
be resolved: that module might export a `push` of its own, and §11 reports what
is guaranteed rather than what is likely.

**[D] The name `push` may be taken.** §14's reference program calls
`push(h, img)` to push an *image to a host*. Nothing has to give: whichever
module supplies that one shadows the builtin for unqualified calls, `check`
warns at the `use`, and `::push` reaches the builtin.

The formatter is unaffected: they are ordinary calls.

## Loop control: `break()` and `continue()`

Both are ordinary zero-argument builtins. They return `:break` and `:continue`.
When left unconsumed, the result reaches the nearest loop in the current
function. Consumed results remain ordinary atoms. A helper must explicitly
return a control atom for its caller to handle it. Use these calls as the
idiomatic way to control loops; see the language specification §9.6.

## Rejection: `reject(msg = :null)`

Returns the single list `[:reject, msg]`. It only constructs the value;
consuming it keeps it as data. Left unconsumed inside a function, it returns
that rejection to overload resolution. Candidates are tried without printing
any refusal. When none accepts and the rejection goes unhandled, the standard
handler reports all refusal messages, including nested helper refusals, on
stderr and fails the call. A successful fallback discards them silently.

## Process control: `exit(code = 0)` and `panic(msg)`

These ordinary, shadowable builtins only construct values:

```hydra
fn exit(code = 0)
    return [:exit, code]
end
fn panic(msg)
    return [:panic, msg]
end
```

A binding, argument, collection, or explicit return consumes the list without
invoking its handler. Any unconsumed **two-element** list of either shape
invokes the standard handler, including a list returned by a helper.

- `[:exit, code]` stops the program without a crash diagnostic and returns the
  requested process status. The handler requires an integer from 0 through 255.
- `[:panic, msg]` stops the program, renders the message on stderr with its
  source location and call trace, and returns status 1.

These handlers do not retry overloads. They stop sibling work, including
in-flight calls, as part of program termination. An already cancelled trail
cannot request a global exit; a panic there follows normal dead-trail crash
isolation (`--strict` still makes dead-trail crashes fatal).

The Rust embedding API returns `RunResult.exit_code` or `RunResult.crash`;
it never exits or panics the host process. `exit()` defaults to status 0.
Use `::exit` and `::panic` to reach these builtins past shadows.
