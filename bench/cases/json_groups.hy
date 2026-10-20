use env
use fs
use json
use list

records := json::parse(fs::read(env::args()[0]))
teams := {}
names := []
for record in records
	team := record.owner.team
	key := :"\(team)"
	if not has(teams, key)
		teams[key] = { :count : 0, :score : 0, :active : 0, :tags : {}, :owners : {} }
		do push(&names, team)
	end

	teams[key].count += 1
	teams[key].score += record.score
	if record.active
		teams[key].active += 1
	end

	for tag in record.tags
		t := :"\(tag)"
		if has(teams[key].tags, t)
			teams[key].tags[t] += 1
		else
			teams[key].tags[t] = 1
		end
	end

	teams[key].owners[:"\(record.owner.id)"] = :true
end

out := []
for name in list::sorted(names)
	group := teams[:"\(name)"]
	books := get(group.tags, :books, 0)
	do push(&out, "\(name):\(group.count):\(group.score):\(group.active):\(len(group.tags)):\(books):\(len(group.owners))")
end

print list::join(out, " ")
