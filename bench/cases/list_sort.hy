use env
use json
use list

n := json::parse(env::args()[0])
// A linear congruential generator, so both languages sort the same numbers.
seed := 1
xs := []
i := 0
while i < n
	seed = seed * 48271 % 2147483647
	do push(&xs, seed)
	i += 1
end

sorted := list::sorted(xs)
print "\(sorted[0]) \(sorted[n / 2]) \(sorted[n - 1])"
