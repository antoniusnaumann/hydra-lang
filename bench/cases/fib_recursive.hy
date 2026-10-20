use env
use json

fn fib(n)
	if n < 2
		return n
	end

	return fib(n - 1) + fib(n - 2)
end

print fib(json::parse(env::args()[0]))
