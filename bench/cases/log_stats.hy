use env
use fs
use json
use list
use text

statuses := {}
status_names := []
paths := {}
path_names := []
requests := 0
for line in fs::lines(env::args()[0])
	f := text::split(line, " ")
	status := :"\(f[4])"
	if has(statuses, status)
		statuses[status] += 1
	else
		statuses[status] = 1
		do push(&status_names, f[4])
	end

	path := :"\(f[3])"
	bytes := json::parse(f[5])
	ms := json::parse(f[6])
	if has(paths, path)
		paths[path].hits += 1
		paths[path].bytes += bytes
		if ms > paths[path].slowest
			paths[path].slowest = ms
		end

	else
		paths[path] = { :hits : 1, :bytes : bytes, :slowest : ms }
		do push(&path_names, f[3])
	end

	requests += 1
end

counts := []
for name in list::sorted(status_names)
	do push(&counts, "\(name)=\(statuses[:"\(name)"])")
end

heaviest := ""
most := -1
slow := 0
for name in list::sorted(path_names)
	stats := paths[:"\(name)"]
	if stats.bytes > most
		most = stats.bytes
		heaviest = name
	end

	slow += stats.slowest
end

print "\(requests) \(text::join(counts, ",")) \(heaviest) \(most) \(slow)"
