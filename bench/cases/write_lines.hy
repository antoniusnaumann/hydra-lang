use env
use fs
use json
use text

n := json::parse(env::args()[0])
out := env::args()[1]
lines := []
i := 0
while i < n
	do push(&lines, "line \(i): value \(i * i % 9973)")
	i += 1
end

_, bytes := fs::write(out, text::join(lines, "\n"))
print "\(bytes) \(len(fs::lines(out)))"
