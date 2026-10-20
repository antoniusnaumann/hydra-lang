use env
use fs
use json
use list
use text

config := {}
sections := []
current := ""
keys := 0
for raw in fs::lines(env::args()[0])
	line := text::trim(raw)
	if line == "" or text::starts_with(line, ";") or text::starts_with(line, "#")
		continue()
	end

	if text::starts_with(line, "[")
		current = text::replace(text::replace(line, "[", ""), "]", "")
		config[:"\(current)"] = {}
		do push(&sections, current)
		continue()
	end

	parts := text::split(line, "=")
	key := text::trim(parts[0])
	value := text::trim(text::join(list::skip(parts, 1), "="))
	config[:"\(current)"][:"\(key)"] = value
	keys += 1
end

ports := 0
enabled := 0
timeouts := 0
for name in sections
	section := config[:"\(name)"]
	ports += json::parse(section.port)
	if section.enabled == "true"
		enabled += 1
	end

	timeouts += json::parse(section.timeout)
end

print "\(len(sections)) \(keys) \(ports) \(enabled) \(timeouts)"
