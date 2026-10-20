use env
use fs
use text

hits := 0
for line in fs::lines(env::args()[0])
	if text::find(line, "error") != :null
		hits += 1
	end
end

print hits
