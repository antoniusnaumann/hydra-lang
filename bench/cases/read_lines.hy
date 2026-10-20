use env
use fs

count := 0
chars := 0
longest := 0
for line in fs::lines(env::args()[0])
	count += 1
	size := len(line)
	chars += size
	if size > longest
		longest = size
	end
end

print "\(count) \(chars) \(longest)"
