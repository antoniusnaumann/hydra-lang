use env
use fs
use json
use text

limit := json::parse(env::args()[1])
counts := {}
words := 0
best := 0
seen := 0
for line in fs::lines(env::args()[0])
	if seen == limit
		break()
	end

	seen += 1
	for word in text::split(line)
		key := :"\(word)"
		if has(counts, key)
			counts[key] += 1
		else
			counts[key] = 1
		end

		words += 1
		if counts[key] > best
			best = counts[key]
		end
	end
end

print "\(words) \(len(counts)) \(best)"
