// Rejected candidates stay silent when a later candidate accepts.
fn choose(value)
	return "accepted"
end
fn choose(value)
	return [:reject, "the middle candidate declined"]
end
fn choose(value)
	reject("the newest candidate declined")
end
print(choose(1)) // accepted; no refusal messages on either output stream.

// The builtin constructs a single list, and consuming it keeps it as data.
result := reject("just data")
print(result[0], terminator = ": ")
print(result[1])
