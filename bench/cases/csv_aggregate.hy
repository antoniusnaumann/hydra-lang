use env
use fs
use json
use text

totals := {}
rows := 0
flagged := 0
first := :true
for line in fs::lines(env::args()[0])
	if first
		first = :false
		continue()
	end

	fields := text::split(line, ",")
	key := :"\(fields[1])"
	amount := json::parse(fields[2])
	if has(totals, key)
		totals[key] += amount
	else
		totals[key] = amount
	end

	if fields[3] == "1"
		flagged += amount
	end

	rows += 1
end

print "\(rows) \(len(totals)) \(totals.books) \(flagged)"
