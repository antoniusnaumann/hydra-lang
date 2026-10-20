use env
use json

n := json::parse(env::args()[0])
counts := {}
i := 0
while i < n
	key := :"k\(i * 7919 % 1000)"
	if has(counts, key)
		counts[key] += 1
	else
		counts[key] = 1
	end

	i += 1
end

// Dicts are not iterable, so walk the keys the loop above produced.
total := 0
j := 0
while j < 1000
	total += counts[:"k\(j)"]
	j += 1
end

print "\(len(counts)) \(total) \(counts.k0)"
