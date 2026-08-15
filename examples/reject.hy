// reject.hy — a function that looks at the values and hands the call back
// (spec §3).
//
//	hydra run examples/reject.hy

// A signature says what a function can be *given*. Only the body can say what
// it can be *used for*, so two functions may share a name and a shape as long
// as the later one can hand a call back.

fn parse(text)
	return "the careful one read \(len(text)) characters"
end

fn parse(text)
	if len(text) > 3
		reject("this one only does short ones")
	end
	return "the quick one read \(text)"
end

print(parse("ab"))
print(parse("abcdef"))

// It reaches every way a call can be written, including through a dot.
print("xy".parse())

// The messages are what a crash prints when nobody takes the call. Both of
// these reject, so this one would end the program with
//
//	crash: no `strict` took 1 argument(s)
//	  strict(n) rejected it: negative numbers are not for me either
//	  strict(n) rejected it: I only take numbers above ten
//
// fn strict(n)
//	reject("I only take numbers above ten")
// end
// fn strict(n)
//	reject("negative numbers are not for me either")
// end
// print(strict(1))

// Rejecting costs nothing: holding the arguments for the fall-back is a handle
// and not a copy. A *write* is what costs — it splits a node the next candidate
// still needs, and the call throws it away — so `check` warns about this shape
// and not about the one above:
//
// fn keen(box)
//	box.tried = .true    // splits a copy…
//	reject("not mine")   // …that nobody wanted
// end

// A shape of its own needs no `reject()` at all: resolution has already told
// the two of them apart (§3).
fn describe(thing)
	return "one thing"
end

fn describe(thing, detail)
	return "one thing, and \(detail)"
end

print(describe("a"))
print(describe("a", "a note"))
