// Run: cargo run -- run tests/fixtures/loop_control.hy
// Explicit return lets a helper offer control to the caller's loop.
fn break_if(condition)
	if condition
		return :break
	end
end

total := 0
for n in [1, 2, 3, 4, 5]
	if n == 2
		continue() // Idiomatic builtin: returns :continue to this loop.
	end
	break_if(n == 4)
	total += n
end
print(total) // 4

// Consuming a control atom keeps it as data.
signal := break()
print(signal) // :break

// Single newlines also join chains on dicts whose keys are atoms.
record := { :name : { :value : "Hydra" }, :status : :ready }
name := record
.name
.value
print(name) // Hydra

// Parallel continue finishes only its own iteration.
sum := 0
parallel for n in [1, 2, 3]
	if n == 2
		continue()
	end
	sum += n
end
print(sum) // 4

// Parallel break stops spawning and cancels the sibling iterations.
parallel while :true
	break()
end
print("done")
