# Hydra — auto-channels

Every trail of a `parallel` or `race` block can hand values to its siblings
without anyone declaring a channel: the block *is* the channel set. Two calls,
`send` and `receive`, and two language features underneath them — variadic
parameters and multiple return values.

Tagged like the handoff: **[D]** decided, **[P]** proposed and cheap to change.
Nothing is left open — §7 records what was ruled and what it costs.

---

## 1. The two calls

```hydra
fn send(value, to*, mode = :wait)   // -> :true / :false
fn receive(from*)                   // -> value, ch
fn channel()                        // -> this trail's own index
```

**[D]** `to*` and `from*` are variadic: zero or more trail indices. Omit them
and it is "any". `mode` sits after a variadic, so it is keyword-only — there is
no way to pass it positionally, and no way to mistake it for an index.

**[D]** Indices are absolute column numbers within the block, counting from 0.

**[D]** `receive` returns **two** values: what was sent, and which trail sent
it. The second may be ignored — `msg := receive()` is the ordinary case.

**[D] When no eligible trail can send any more, `receive` returns `:null`.**
The channel comes back `:null` too, and *that* is the unambiguous signal: a
trail may legitimately send `:null`, but it always arrives with a real index
behind it. So `ch == :null` means closed and nothing else can.

---

## 2. A handoff

```hydra
parallel
	send("ready") || msg := receive()
	              || print("got \(msg)")
end
```

Trail 0 offers a value and waits; trail 1 takes it and drops the channel it came
from. Rows stay cosmetic — the send is what synchronises, not the row boundary.

---

## 3. Both ends address the other

```hydra
parallel
	job := next() || work := receive(0) || work := receive(0)
	send(job, 1)  || run(work)          || run(work)
	send(job, 2)  ||                    ||
end
```

**[P]** `send`'s indices are **destinations**; `receive`'s are **sources**. A
trail never names itself: `send(v, 1)` from trail 1 is a crash, and a bare
`send(v)` / `receive()` never matches the caller's own trail.

---

## 4. First to receive wins, and knows when to stop

```hydra
parallel
	for job in JOBS || while alive()
	send(job)       || work, ch := receive()
	end             || if ch == :null
	                || break()
	                || end
	                || run(work)
	                || end
end
```

One producer, one worker, no queue and no pool object: `send(job)` with no index
goes to whichever trail asks first. The worker leaves its loop when the channel
comes back `:null`, which is the only thing a producer cannot fake.

The producer can tell too, because `send` says so:

```hydra
parallel
	while send(item()) || while alive()
	count = count + 1  || run(receive())
	end                || end
end
```

`send` returns `:false` once every trail that could have received has ended.

---

## 5. The three modes

```hydra
parallel
	send(cfg, mode = :broadcast) || apply(receive()) || apply(receive())
end

parallel
	send(metric, mode = :detach) || later := receive()
	go_on()                      || print(later)
end
```

**[D]** `:wait` (the default) blocks until someone receives, and returns `:true`
when they did or `:false` when nobody is left to.
**[D]** `:detach` buffers and returns immediately.
**[D]** `:broadcast` buffers one copy for every eligible trail — all of them, or
just the ones named. `:detach` and `:broadcast` return `:false` only when every
eligible trail has already ended.
**[D]** Buffers are FIFO per (sender, receiver) and unbounded, and a broadcast
is buffered only for trails that have been spawned. A `:detach` producer can
therefore outrun its consumers without limit; the alternative is a bound, and a
bound turns `:detach` back into something that blocks.

---

## 6. What the two calls need from the language

### 6.1 Variadic parameters **[D]**

```hydra
fn log(prefix, values*, sep = " ")
	for v in values
		print("\(prefix)\(sep)\(v)")
	end
end

log("start")                  // values is []
log("hit", 1, 2)              // values is [1, 2]
log("hit", 1, 2, sep = ", ")  // sep can only be named
```

`name*` collects the remaining positional arguments into a **list**, and
everything declared after it is **keyword-only**. A bare `*` takes no arguments
and exists only to close the positional list:

```hydra
fn retry(host, *, attempts = 3)
	print("\(host) x\(attempts)")
end

retry("eu", attempts = 5)  // fine
retry("eu", 5)             // crash: no candidate accepts two positional arguments
```

**[P]** Parameter order becomes: required, defaulted, variadic, keyword-only.
This restates §3's "parameters with defaults come after those without".

**[D] A concrete arity beats a variadic.** §3 resolves a call by taking the
first candidate that accepts it, innermost binding first. A variadic candidate
never rejects on arity, so it would swallow every narrower one behind it.
Instead, candidates that can take the call *exactly* are tried first, in the
usual order, and only then the variadic ones:

```hydra
fn f(a)
	return "one"
end
f := fn(rest*) "many"
f(1)     // "one"  — the concrete arity wins, though the shadow is nearer
f(1, 2)  // "many" — nothing else accepts two
```

### 6.2 Multiple return values **[D]**

```hydra
value, ch := receive()
value, ch = receive()   // assignment too
```

**Any function may return several values**, not just the two builtins:

```hydra
fn parse(text)
	return decode(text), len(text)
end

value := parse(s)         // extras dropped, silently
value, size := parse(s)   // both named
value, size, why := parse(s)  // crash: `parse` returns 2 values, 3 were named
```

**[D] By convention the first value is the meaningful one and the rest are
additional information**, so dropping them is always safe to write and never
changes what the first one means.

**[D]** Extras are dropped in silence; naming *more* than arrive is a **crash**.
**[D]** A multi-value is **not a value**: it exists only between a call and a
binding site. It cannot go into a list, a dict, or an argument.

This is what makes §1's `:null` rule work: the value and the channel are
separate answers, so "closed" is knowable without stealing a value from the
program.

### 6.3 `channel()` — the trail's own index **[D]**

```hydra
parallel for job in JOBS
	send(result(job), channel() + 1)
end
```

`channel()` answers the calling trail's own index, which is what makes a
pipeline writable — `receive` says where a value came from, and `channel()` says
where "here" is. It is lexically scoped like `send` and `receive` (§6.4), so
outside a trail it is not a call that returns something odd; it is a `check`
error.

### 6.4 Lexical scope **[D]**

`send`, `receive` and `channel` may appear only **syntactically inside** a `parallel` or
`race` body. `check` rejects them anywhere else, including inside a function the
trail calls. They are keyword-like, not function-like.

This is a deliberate contrast with `alive()`, which is dynamic at any call depth
(§9.5) — a `send` buried three frames down is exactly the shared-state surprise
`&` was made visible to prevent.

### 6.5 Cancellation unblocks a parked call **[D]**

```hydra
race
	send(:ready) || v := receive()
	             || print(v)
end
```

Trail 0 finishes, the race is decided, trail 1 is cancelled — while parked in
`receive`. It returns `:null`, `:null`, and the trail runs no further statement.

Conceptually `receive` checks `alive()` while it waits; it is a builtin, so it
does not really, but that is the rule. This is the one place §9.5's "a cancelled
trail is never interrupted" bends, and it bends where nothing can observe it.

### 6.6 Everybody waiting is a crash **[D]**

A block whose every live trail is parked, with nothing buffered, can never make
progress. That is a crash — "every trail in this block is waiting" — not a hang.
The runtime already knows how many trails are parked, so it is cheap to see, and
a hang teaches nobody anything.

### 6.7 An index that cannot exist **[D]**

```hydra
parallel
	send(v, 2) || j := receive()
end
// error: no trail 2: this block has 2 trails, 0 and 1
```

A crash at run time, and a `check` error whenever the block's trail count is
known at compile time — which is the row form always, and the others never
(§7.1).

---

## 7. What was ruled, and what it costs

### 7.1 A `parallel for` / `parallel while` is indexed by spawn order **[D]**

Trail 0 is the first spawned, and the number climbs. `send(v)` and `receive()`
work unchanged; an explicit index is only meaningful to a program that knows how
many trails it started, and `check` can verify none of them — the row form is
the only shape whose trail count is known before the program runs. An index that
cannot exist is still a crash when the block has stopped spawning (§6.7).

### 7.2 Sending a reference **[P]**

Everything copies (§5.1), so `send(rows)` hands over a deep copy and the sender
keeps its own. `send(&rows)` is legal and is the one way to put shared mutable
state through a channel — the same reading `&` already has at every call site,
and `check`'s `ref-into-trail` warning covers it.

### 7.3 No spread at the call site **[P]**

`name*` collects, but nothing scatters: with a list of indices in hand there is
no way to say `send(v, indices*)`. Left out for now — the channel calls take
literal indices, and adding it later is additive.

### 7.4 `send`, `receive` and `channel` as names **[P]**

Reserved inside a block body and ordinary outside it, which is the rule `trail`
already lives by as a label.

### 7.5 A cancelled trail can still deliver **[D, follows from §9.5]**

Cancellation is value-level, not effect-level: a losing trail can still finish
sending the request or charging the card. A `send` is an effect, so a losing
trail's already-delivered value stays delivered and its receiver acts on it.

---

## 8. What it costs the tools

- **Lexer / parser.** `name*` and a bare `*` in a parameter list; a comma list
  of targets on the left of `:=` and of `=`; `return a, b`. `*` is already an
  operator, but a parameter list can hold no expression, so there is nothing to
  disambiguate.
- **Formatter.** `values*` with no space, `*` alone as its own parameter,
  `value, ch := receive()` spaced like any other comma list.
- **check.** The §6.7 index error; `send` / `receive` / `channel` outside a
  block (§6.4); a positional argument after a variadic; the restated parameter
  order; naming more values than a call can return, where that is knowable; and
  the resolution change of §6.1, which alters which candidate a call picks and
  so touches every diagnostic that lists what was tried.
- **Interpreter.** A per-block mailbox set, park/unpark on the run queue, the
  unblock-on-cancel path (§6.4), the all-parked crash (§6.5), and multiple
  return values through the evaluator.
- **tree-sitter / editor.** The parameter rule, the multi-target declaration,
  and `send` / `receive` joining the builtin list for highlighting.
