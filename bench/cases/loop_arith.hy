use env
use json

n := json::parse(env::args()[0])
i := 0
total := 0
while i < n
	total += i % 7 * 3 - i % 5
	i += 1
end

print total
