use env
use json

n := json::parse(env::args()[0])
xs := []
i := 0
while i < n
	do push(&xs, i * 2)
	i += 1
end

print "\(len(xs)) \(xs[n - 1])"
