use env
use fs
use json
use list
use text

lines := fs::lines(env::args()[0])
header := text::split(lines[0], " ")
n := json::parse(header[0])
adjacent := list::repeat([], n)
i := 1
while i < len(lines)
	edge := text::split(lines[i], " ")
	a := json::parse(edge[0])
	b := json::parse(edge[1])
	do push(&adjacent[a], b)
	do push(&adjacent[b], a)
	i += 1
end

distance := list::repeat(-1, n)
distance[0] = 0
queue := [0]
head := 0
while head < len(queue)
	node := queue[head]
	head += 1
	for next in adjacent[node]
		if distance[next] == -1
			distance[next] = distance[node] + 1
			do push(&queue, next)
		end
	end
end

farthest := 0
total := 0
for d in distance
	if d > farthest
		farthest = d
	end

	if d > 0
		total += d
	end
end

print "\(n) \(len(queue)) \(farthest) \(total)"
