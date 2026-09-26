// The outer call can omit parentheses; a call used as an argument keeps them.
fn add(a, b)
	return a + b
end

sum := add 20, len([1, 2])
print sum

// Whitespace distinguishes a negative argument from subtraction.
negative := add -1, 3
print negative

// return_if forwards a request; its caller's function handles it.
fn return_if(condition, value)
	if condition
		return return(value)
	end
end

fn choose()
	return_if :true, 42
	return 0
end

print choose()

// Consuming a return request keeps it as ordinary data.
request := return :ready
print request
