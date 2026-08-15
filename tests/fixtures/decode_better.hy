// The later `use` is tried first, and hands back what it will not take (§3).
fn decode(text)
	if text == "hard"
		reject("the better decoder only does easy ones")
	end
	return "better: \(text)"
end
