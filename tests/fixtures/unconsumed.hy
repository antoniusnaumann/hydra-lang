// What a statement's result does when nothing consumes it (§8.1). The CLI
// test compares this file's output line by line.

fn pair()
	return 1, 2
end

fn quiet()
	// Inside a function an ordinary result is dropped.
	pair()
	42
end

// At the top level it is printed, all of it on one line.
pair()
"text"
3 + 4

// Nothing: `print` answers `:null`, and so does a function without `return`.
quiet()

// `_` consumes it, and so does any binding.
_ = pair()
first := pair()

// The extras belong to the call that made them: a statement after it that is
// not a call prints only its own value.
first

// `if` and `for` are statements, so the bodies are statement level too.
for i in [1, 2]
	i * 10
end
if first == 1
	"branch"
end
