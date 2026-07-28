# Hydra — standard library, draft 1

**Status: proposed, not implemented.** This is the implementor's draft of the
five builtins the language owner asked for. Nothing here is in the interpreter
yet.

Same tags as the handoff: **[D]** decided, **[P]** proposed, **[O]** open.

---

## 1. What a builtin is

**[P]** These five are **global names**, not a module. They are in scope in
every file without a `use`, and they are *not* reachable through `::` — there is
no module for them to belong to.

**[P]** They are looked up **after the scope chain**, so a program may shadow
one: `print := fn(x) … end` wins for the rest of that scope, exactly as §6's
outward walk implies. That keeps them from being reserved words.

**[D]** They are ordinary values. `f := print` binds it, `f("hi")` calls it.

**[D]** Arity is exact, as for any call (§8). There are no optional arguments,
which is why `get` takes its default rather than defaulting it.

---

## 2. `print(value)` → `.null`

**[P]** Writes the **text form** of `value` — the same rendering `\(value)`
produces (§1) — followed by a newline, to standard output.

One argument, not many: interpolation already composes, so `print("a \(b) c")`
covers what a variadic `print` would, and Hydra has no variadic calls.

**[D]** Returns `.null`, so it is a statement, not an expression to build with.

**[P] Ordering under concurrency.** Output is written in the order trails
actually run, which is the scheduler's order and not the order of the columns.
Two trails printing interleave by line, never mid-line: one `print` is one write.

**[O]** Whether there is a matching `eprint` for stderr, and whether `print`
should flush. Both matter for a language whose crashes go to stderr.

---

## 3. `has(container, key)` → `.true` / `.false`

**[P]** For a **dict**, whether `key` — a symbol — is present.
For a **list**, whether `key` — a number — is an index the list has, with the
negative-index rule of §5 applied first, so `has(xs, -1)` is `.false` only on an
empty list.

**[D]** Crashes if `container` is neither a dict nor a list, or if the key's kind
does not match the container's. Asking about the wrong kind of thing is a bug,
not a `.false`.

This is the function §15.1 says every program touching decoded data needs on its
first line, because a missing-key *read* crashes and nothing can be caught.

---

## 4. `get(container, key, fallback)` → value

**[P]** The value at `key`, or `fallback` when it is missing. Same container and
key rules as `has`.

**[D]** `fallback` is **required**. With exact arity there is no way to make it
optional, and requiring it makes the missing case visible at the call site.

**[D]** The result copies, like every other read (§5.1). `get(d, .k, [])` hands
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

**[D] The first argument must be a reference.** `push(rows, x)` — without the
`&` — crashes.

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

**[P]** This makes `push` the shape every future mutating helper follows:
**a mutating helper takes `&` first and says so by crashing.** Deciding that now
is what §15.7 means when it says the stdlib's signatures determine where `&`
appears in ordinary code.

---

## 7. What this unblocks, and what it does not

With these five, §14's reference program still needs `read_file`, `lease`,
`push`(✓), `wait_ready`, `healthy`, `drain`, `smoke`, `live`, `rollback` and
`json::decode`. What it does unblock is everything a *test* needs: a program can
finally report what it computed, and decoded data can be read without crashing.

**[O]** Still absent and still needed by ordinary code: string helpers (there is
no way to split, trim, or compare case-insensitively), list helpers beyond
`push`, and any I/O at all. Each one wants the same `&`-first decision `push`
just made.

---

## 8. `check` and the formatter

**[P]** `check` gains three rules, all of which fit §11's "only what is
guaranteed":

| Diagnostic | Basis |
|---|---|
| `push` called with a non-reference first argument | the signature above — a guaranteed crash |
| arity mismatch against any of the five | they are statically known functions |
| shadowing one of the five with a different arity | the call that follows will crash |

The five stop being undeclared names, so `hydra check --extern print,…` is no
longer needed for them.

The formatter is unaffected: they are ordinary calls.
