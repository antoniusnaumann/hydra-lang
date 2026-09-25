// Run: cargo run -- run tests/fixtures/continuation_boundaries.hy
a := 2

// The blank line makes these two naked expressions: 32, then -2.
32

-a

// Colon atoms start fresh statements even without a blank line.
foo := { :bar : 1 }
copy := foo
:bar
print(copy.bar) // 1

// A control atom also starts a statement directly after a block header.
fn bare_rejection()
	if :true
		:reject
	end
end
result := bare_rejection()
print(result) // :reject

// The idiomatic builtin call needs no separating blank line.
fn builtin_rejection()
	if :true
		reject()
	end
end
result = builtin_rejection()
print(result) // [:reject, :null]
