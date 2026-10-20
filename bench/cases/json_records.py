import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    records = json.load(f)
score = 0
tags = 0
active = 0
for record in records:
    score += record["score"]
    tags += len(record["tags"])
    if record["active"]:
        active += 1
# Compare the round trip, not the text: number formatting differs.
again = json.loads(json.dumps(records, separators=(",", ":"), ensure_ascii=False))
print(f"{len(records)} {score} {tags} {active} {len(again)}")
