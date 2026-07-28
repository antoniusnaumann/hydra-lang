// trails.hy — what the language does without a standard library.
//
// There is no `print`: `alive()` is the only primitive the spec defines, so a
// program cannot yet say what it computed (QUESTIONS.md §1). Run it with
// `hydra run examples/trails.hy --dump-scope` to see the bindings it leaves.

// Value semantics: everything copies, `&` opts out (§5.1).
original := { .count : 0, .tag : .fresh }
copy := original
copy.count = 1
untouched := original.count // 0 — the copy split on write

fn bump(box)
	box.count = box.count + 1
end

bump(original) // a copy, so the caller is safe
by_value := original.count // still 0
bump(&original) // a reference: the caller marked it
by_reference := original.count // 1

// Identity is the copy-on-write buffer, for now (§5.1).
same_buffer := copy === original // .false, they have split
self_same := original === original // .true

// Trails share the parent scope; `:=` inside one is trail-local (§6, §9.2).
first := .null
second := .null
third := .null

parallel
	first = "eu" || second = "us"             || third = "ap"
	             || second = "\(second)-east" ||
end

regions := [first, second, third]

// A race is decided by the first trail to finish, and the losers are cancelled
// before their next statement (§9.4, §9.5).
winner := .null
race
	winner = "short" || slow := 1
	                 || slow = slow + 1
	                 || winner = "long"
end

// `alive()` is dynamic: any function can ask, at any depth (§9.5).
outside := alive()
