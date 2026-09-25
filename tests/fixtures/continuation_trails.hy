// Run: cargo run -- run tests/fixtures/continuation_trails.hy
// The function discards standalone values in trails, keeping output deterministic.
fn show_trails()
	foo := { :bar : { :baz : 42 } }
	left := 0
	right := 0

	// Continuation follows each column, never the neighboring trail.
	parallel
		left = foo || right = 32
		.bar       || -2
		.baz       || * 3
	end
	print("\(left), \(right)") // 42, 26

	// A blank physical line stops continuation in both trails.
	parallel
		left = 32 || right = 40

		-2        || -3
	end
	print("\(left), \(right)") // 32, 40

	// An empty cell stops continuation only in its own trail.
	parallel
		left = 32 || right = 40
		          || -2
		-3        || -4
	end
	print("\(left), \(right)") // 32, 34
end

show_trails()
