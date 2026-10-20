use env
use json
use list

n := json::parse(env::args()[0])
xs := list::range(n)
total := 0
for x in xs
	if x % 3 == 0
		total += x
	end
end

print total
