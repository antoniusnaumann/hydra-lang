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

// `reject(msg)` is `return [:reject, msg]`: it leaves the function because a
// `[:reject, msg]` that nothing consumes returns from its function (§8.1).
// That is what lets a helper reject on its caller's behalf, through any
// number of calls, with nothing written to pass it on.
fn reject_if(cond, why)
	if cond
		return [:reject, why]
	end
end

fn describe_number(n)
	return "some number"
end

fn describe_number(n)
	reject_if(n < 0, "negative numbers go to the general one")
	return "a non-negative number"
end

print(describe_number(4))
print(describe_number(-4))

// Consumed, a rejection is a value like any other.
result := reject_if(:true, "just looking")
verdict := result[0]
why := result[1]
print("\(verdict) because \(why)")

// When every candidate rejects, the call answers with the rejection. Left
// unconsumed at the top level it ends the program, listing every refusal:
//
//	crash: unhandled rejection
//	  the top level of a file has no call to hand it back to
//	  no `strict` took 1 argument(s)
//	    strict(n) rejected it: negative numbers are not for me either
//	    strict(n) rejected it: I only take numbers above ten
//
// fn strict(n)
//	reject("I only take numbers above ten")
// end
// fn strict(n)
//	reject("negative numbers are not for me either")
// end
// strict(1)

// Rejecting costs nothing: holding the arguments for the fall-back is a handle
// and not a copy. A *write* is what costs — it splits a node the next candidate
// still needs, and the call throws it away — so `check` warns about this shape
// and not about the one above:
//
// fn keen(box)
//	box.tried = :true    // splits a copy…
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
