// With no accepting candidate, the standard handler fails and reports every
// refusal, including refusals reached through a helper. Nothing is printed early.
fn inner(value)
	return [:reject, "inner fallback declined"]
end
fn inner(value)
	reject("inner newest declined")
end
fn choose(value)
	reject("outer fallback declined")
end
fn choose(value)
	inner(value)
end
choose(1)
print("unreachable")
