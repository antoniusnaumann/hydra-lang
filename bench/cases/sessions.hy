use env
use fs
use json
use list
use text

// A session ends when its client has been quiet for this many seconds.
gap := 300
visits := []
for line in fs::lines(env::args()[0])
	f := text::split(line, " ")
	do push(&visits, [f[1], json::parse(f[0])])
end

ordered := list::sorted(visits)
sessions := 0
longest := 0
length := 0
client := ""
last := 0
for visit in ordered
	if visit[0] != client or visit[1] - last > gap
		sessions += 1
		length = 0
		client = visit[0]
	end

	length += 1
	if length > longest
		longest = length
	end

	last = visit[1]
end

print "\(len(ordered)) \(sessions) \(longest)"
