use env
use fs
use json

records := json::parse(fs::read(env::args()[0]))
score := 0
tags := 0
active := 0
for record in records
	score += record.score
	tags += len(record.tags)
	if record.active
		active += 1
	end
end

// Compare the round trip, not the text: number formatting differs.
again := json::parse(json::stringify(records))
print "\(len(records)) \(score) \(tags) \(active) \(len(again))"
