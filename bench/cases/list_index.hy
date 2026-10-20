use env
use json
use list

n := json::parse(env::args()[0])
xs := list::range(n)
i := 0
while i < n
	xs[i] = xs[i] * 3 % 11
	i += 1
end

total := 0
i = 0
while i < n
	total += xs[i]
	i += 1
end

print total
