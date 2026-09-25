# Hydra — `fs`

The filesystem module. `use fs` brings it in for qualified calling — `fs::read`
— and `use fs as *` binds the names bare for a file that wants them (§7).

**`fs` is a built-in module**: the interpreter and `check` know it exists, and
its functions are namespaced builtins rather than a file anyone can find. A file
named `fs.hy` beside the program shadows it, which is why doing that is
discouraged and why `check` says so.

Nothing here is implemented yet. §1 is the one rule the whole shape rests on;
§2–§6 are the signatures.

---

## 1. Three rules, and then nothing to remember

**[P] Defaults absorb the ordinary failures.** Writing to a directory that does
not exist creates it. Making a directory that already exists is fine. Removing
something that is not there answers `:false` rather than failing. These are the
cases every script hits, and every one of them is a default rather than a
paragraph in the documentation.

**[P] Anything left crashes.** A denied permission, a full disk, a file that is
not there when you asked to read it — those end the program with a diagnostic,
because there are no exceptions (§8) and a silent `:null` reaching the next line
is how a script destroys data.

**[D] A reader opts out with a `fallback`, and then says why.** This is exactly
the shape `get(container, key, fallback)` already has for dicts (stdlib §4), and
it is the same story: reading what is not there crashes, and asking for it
safely is one argument.

```hydra
config := read("config.json")            // crashes if it is not there
config := read("config.json", "{}")      // takes the fallback
text, why := read("maybe.txt", "")       // and says :not_found
```

**[D] Every reader is two functions, not one with a sentinel.** The safe form is
an overload, and resolution by shape picks it (§3): one argument means the
crashing one, two means the falling-back one. Nothing stands in for "no fallback
given", so there is no value a program cannot fall back to.

**[D]** The falling-back form answers with a second value: `:null` when it did
not have to fall back, and the reason when it did. Extras are dropped in silence
(channels §6.2), so nobody who does not care ever has to look.

**[D] Every flag is keyword-only.** A `*` closes the positional list before them
(channels §6.1), which is what keeps the two overloads apart no matter which
order they are declared in — without it, `list(dir, "x")` could fill `match` or
`fallback` depending on which candidate happens to be nearer.

---

## 2. Paths

Pure string functions. No I/O, so none of them can fail.

```hydra
join(base, parts*)      // "src", "vm", "mod.hy" -> "src/vm/mod.hy"
parent(path)            // "src/vm/mod.hy" -> "src/vm"
name(path)              // -> "mod.hy"
stem(path)              // -> "mod"
extension(path)         // -> "hy", with no dot
absolute(path)          // -> "/home/a/src/vm/mod.hy"
```

`join` is variadic (channels §6.1), which is what makes it read like the
sentence it is. It also normalises: `join("a/", "/b")` is `"a/b"`.

---

## 3. Asking

```hydra
exists(path)              // -> :true / :false
is_file(path)             // -> :true / :false
is_dir(path)              // -> :true / :false

size(path)                // -> bytes
size(path, fallback)      // -> bytes, why

modified(path)            // -> seconds since the epoch
modified(path, fallback)  // -> seconds, why
```

The three questions never fail: a path that cannot be looked at is not there as
far as the asker is concerned. `size` and `modified` are readers and take a
`fallback` like the rest.

---

## 4. Reading

```hydra
read(path)                // -> text
read(path, fallback)      // -> text, why

lines(path)               // -> list of lines
lines(path, fallback)     // -> list of lines, why

list(dir, *, match = "*", recursive = :false)            // -> paths
list(dir, fallback, *, match = "*", recursive = :false)  // -> paths, why
```

**[P]** `list` answers with **full paths**, joined onto `dir`, because the next
thing anyone does with an entry is read it. They come back **sorted**, so a
program over a directory is reproducible.

**[P]** `match` is a glob with `*` and `?` and nothing else. `recursive` is what
`**` would have been, and having it as a flag keeps one pattern language rather
than two.

```hydra
for entry in list("src", match = "*:hy", recursive = :true)
	print("\(entry.name()) is \(entry.size()) bytes")
end
```

---

## 5. Writing

```hydra
write(path, text, *, mode = :replace, parents = :true)       // -> path, bytes
copy(source, target, *, overwrite = :true, parents = :true)  // -> target
move(source, target, *, overwrite = :false, parents = :true) // -> target
remove(path, *, recursive = :false)                          // -> :true / :false
make_dir(path, *, parents = :true)                           // -> path
```

A writer has no falling-back overload: its ordinary failures are already
absorbed by the defaults above, and what is left — a denied permission, a full
disk — is not something a fallback value can stand in for.

**[P]** `mode` is `:replace`, `:append` or `:new` — `:new` fails when the path
already exists, which is the only way to write a file without a race against
whoever else is writing it.

**[P]** `move`'s `overwrite` defaults to `:false` where `copy`'s defaults to
`:true`, and the difference is deliberate: a copy that clobbers loses a copy, a
move that clobbers loses the only one.

**[P]** `remove` answers whether there was anything to remove, so
`if remove(p)` reads as "if it was there". A directory needs `recursive = :true`
— refusing to delete a tree that was not named as one is worth the one flag.

**[P]** Each of these returns something worth chaining: the path it wrote or the
target it landed on, so `write(p, t).size()` is a sentence.

---

## 6. Where the process is

```hydra
cwd()   // -> the working directory
home()  // -> the user's home
temp()  // -> the directory for temporary files
```

**[P]** There is no `cd`. A process-wide cursor that every trail shares is a
race by construction (§9.2), and `join(cwd(), …)` says the same thing without
one.

---

## 7. Consequences and open points

### 7.1 A path is a string, so the dot already works — **[D, follows from §5.2]**

Nothing in this module needs method syntax, because calling through a dot hands
the receiver to a free function:

```hydra
if "config.json".exists() and not "build".is_dir()
	make_dir("build")
end

total := 0
parallel for file in list("src", match = "*:hy")
	total += file.read().len()
end
```

This is also why the names are lean. `read`, `write`, `list`, `size` and `name`
are only ever read next to their receiver.

### 7.2 Lean names are safe now — **[D, follows from §7]**

`list`, `name`, `size`, `read` and `write` are exactly the names a program might
want for itself, and a plain `use fs` no longer takes any of them: the module
comes in for qualified calling only. A file that wants the bare names asks for
them with `as *` and owns the consequence, which `check` warns about there.

The qualifier reaching through the dot is what keeps that from being verbose:

```hydra
use fs

for entry in cwd().fs::list(match = "*:hy")
	print("\(entry.fs::name()) is \(entry.fs::size()) bytes")
end
```

is the qualified form, and

```hydra
use fs as *

for entry in list(cwd(), match = "*:hy")
	print("\(entry.name()) is \(entry.size()) bytes")
end
```

is the bare one. Both read; neither needs the names to be long.

**None of them shadow a builtin** either, which was a constraint on the whole
list: `print`, `has`, `get`, `len`, `push`, `alive`, `send`, `receive` and
`channel` are all clear of it.

### 7.3 Two overloads, and what they need from `::` — **[D]**

A reader is two functions of the same name, so `fs::read` is a *set* of
candidates rather than one. §7 says a qualified call "names one function, so
there is nothing to fall through to", and that stays true in the sense it was
written: the fall-through it rules out is to *another* module's or the builtin
namespace's function. Within the module, resolution by shape picks among the
module's own candidates exactly as it does for an unqualified name.

This is the one thing the qualified form has to learn, and it is worth stating
in §7 rather than here.

### 7.4 Binary files — **[O]**

Every reader here is text, and text is UTF-8 (§1). A file that is not valid
UTF-8 crashes with `:encoding`, or takes the fallback. There is no byte type in
the language, so binary I/O needs one — a list of numbers is not it — and that
is a language question rather than an `fs` one.

### 7.5 Streaming — **[P] deliberately absent**

`read` and `lines` take the whole file. There is no handle, no `open`, and
nothing to close, because there is no `defer` and no `with`, so a handle would
be a resource nobody can be relied on to release. A file too large to hold is
the one case this module does not serve, and the shape it would want —
`for line in stream(path)` — needs iterators the language does not have.

### 7.6 Errors are a small closed set — **[P]**

`:not_found`, `:denied`, `:exists`, `:is_dir`, `:not_dir`, `:encoding`, `:io`.
The symbol is what a program branches on; the crash message is where the
operating system's own words go.

### 7.7 What it costs the implementation — **[D]**

`fs` is a **built-in module**. `use fs` resolves the way it always did — same
directory, then `HYDRA_PATH` — and falls back to the built-in set, so a file
named `fs.hy` shadows it. That is legal and discouraged, and `check` warns at
the `use` that does it: a shadowed module is the same failure mode as a shadowed
name, and it still returns *something*.

What the implementation needs, then:

- a namespace of native functions that `use` can bind without a file, and that
  `check` knows the signatures of without parsing one;
- qualified resolution by shape among a module's candidates (§7.3);
- the functions themselves, each of which suspends (§7.8).

Nothing here needs a new value kind: paths are strings, and every answer is a
string, a number, a list or a symbol.

### 7.8 It suspends, which is the whole point — **[D, follows from §9.1]**

I/O is a scheduling point, so a trail that reads a file lets its siblings run. A
`parallel for` over a directory is real overlap, not a loop with extra steps.
