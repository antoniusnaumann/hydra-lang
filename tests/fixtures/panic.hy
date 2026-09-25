// A consumed panic list is ordinary data.
pending := panic("something went wrong")
fn fail(value)
	// This unconsumed value invokes the handler, reporting a call trace.
	value
end
fail(pending)
print("unreachable")
