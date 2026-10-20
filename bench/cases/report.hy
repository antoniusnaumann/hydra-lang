use env
use fs
use json
use text

fn pad(value, width)
	out := "\(value)"
	while len(out) < width
		out += " "
	end

	return out
end

records := json::parse(fs::read(env::args()[0]))
lines := ["id     | name         | score | team     | tags"]
for record in records
	if record.active and record.score >= 500
		row := "\(pad(record.id, 6)) | \(pad(record.name, 12)) | \(pad(record.score, 5)) | \(pad(record.owner.team, 8)) | \(text::join(record.tags, ","))"
		do push(&lines, row)
	end
end

table := text::join(lines, "\n")
print "\(len(lines)) \(len(table))"
