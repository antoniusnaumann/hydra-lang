// trails.hy — the parts of the language that need no more than the five
// builtins of spec/hydra_stdlib.md.
//
//	hydra run examples/trails.hy

// Value semantics: everything copies, `&` opts out (§5.1).
original := { .count : 0, .tag : .fresh }
copy := original
copy.count = 1
print("the copy split on write: original is \(original.count), the copy is \(copy.count)")

fn bump(&box)
	box.count = box.count + 1
end

bump(&original)
print("through a reference: \(original.count)")

// `&box` in the signature means the call must mark it. Without the `&` the
// callee would work on a copy, so it crashes instead:
//
// bump(original)   // crash: takes `box` by reference

// Identity is the copy-on-write buffer, for now (§5.1).
print("copy === original: \(copy === original), and original === original: \(original === original)")

// Reading a missing key crashes, so `has` and `get` are how you ask (§15.1).
config := { .region : "eu" }
print("region \(get(config, .region, "?")), retries \(get(config, .retries, 3))")
print("has .retries: \(has(config, .retries))")

// Trails share the parent scope; `:=` inside one is trail-local (§6, §9.2).
first := .null
second := .null
third := .null

parallel
	first = "eu" || second = "us"             || third = "ap"
	             || second = "\(second)-east" ||
end

print("regions: \([first, second, third])")

// A `&` crossing into a trail is the only shared mutable state there is (§9.2),
// and the only way two trails can fill one list.
seen := []
parallel for region in [first, second, third]
	_ = push(&seen, region)
end
print("\(len(seen)) trails reported")

// A race is decided by the first trail to finish, and the losers are cancelled
// before their next statement (§9.4, §9.5).
winner := .null
race
	winner = "short" || slow := 1
	                 || slow = slow + 1
	                 || winner = "long"
end
print("winner: \(winner)")

// `alive()` is dynamic: any function can ask, at any depth (§9.5).
print("outside a trail, alive() is \(alive())")

// A name can mean several functions: a call takes the first that accepts its
// argument count and names, so shadowing one shape leaves the others reachable.
fn describe(region)
	return "region \(region)"
end
describe := fn(region, detail) "region \(region) (\(detail))"

bello := { .name : "Bello", .age : 21 }

lol := { .dog : bello }
lol.dog.name = "Hasso"

print(bello.name)

print(describe("eu"))
print(describe("eu", "primary"))
print(describe(detail = "backup", region = "ap"))
