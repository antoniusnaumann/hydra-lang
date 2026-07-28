// a module that exports decode, like the spec example

fn decode(body)
	return "json:" + body
end

fn _hidden()
	return "secret"
end

_key := 42
