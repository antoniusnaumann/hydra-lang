use env
use json
use list

n := json::parse(env::args()[0])
total := list::range(n)
	.list::map(fn(x) x * x % 1000)
	.list::filter(fn(x) x % 2 == 0)
	.list::sum()
print total
