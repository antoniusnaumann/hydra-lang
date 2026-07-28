// deploy.hy — warm three regions at once, then verify
//
// This is the reference program from spec §14. Most names in it are still
// placeholders awaiting the standard library, so it parses, checks and formats
// but does not run. See QUESTIONS.md §1.
//
// Note `push(h, img)` on line 15: it means "push an image to a host", while
// the standard library's `push(&list, value)` appends to a list. Same name,
// different operation — see hydra_stdlib.md §8.

use fmt
use http
use json // http exports decode too, so json:: disambiguates

REGIONS := ["eu", "us", "ap"]

fn warm(name, img)
	h := lease(name)
	push(h, img)
	wait_ready(h, 60)
	if not healthy(h)
		drain(h)
		return .failed
	end
	return h
end

manifest := json::decode(read_file("deploy.json"))
img := manifest.image // same as manifest[.image]

for name in REGIONS
	print("target \(name) -> \(img)")
end

eu := .null
us := .null
ap := .null

parallel
	eu = warm("eu", img) || us = warm("us", img) || ap = warm("ap", img)
	smoke(eu)            || smoke(us)            || smoke(ap)
end

down := 0
for h in [eu, us, ap]
	if h == .failed or not live(h)
		down = down + 1
	end
end

if down > 0
	rollback()
end
print("\(3 - down)/3 regions live")
