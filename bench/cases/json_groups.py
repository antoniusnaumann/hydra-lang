import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    records = json.load(f)
teams = {}
for record in records:
    team = record["owner"]["team"]
    if team not in teams:
        teams[team] = {"count": 0, "score": 0, "active": 0, "tags": {}, "owners": {}}
    group = teams[team]
    group["count"] += 1
    group["score"] += record["score"]
    if record["active"]:
        group["active"] += 1
    for tag in record["tags"]:
        if tag in group["tags"]:
            group["tags"][tag] += 1
        else:
            group["tags"][tag] = 1
    group["owners"][str(record["owner"]["id"])] = True

out = []
for name in sorted(teams):
    g = teams[name]
    books = g["tags"].get("books", 0)
    out.append(f"{name}:{g['count']}:{g['score']}:{g['active']}:{len(g['tags'])}:{books}:{len(g['owners'])}")
print(" ".join(out))
