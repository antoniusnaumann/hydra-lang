import json
import sys


def pad(value, width):
    out = f"{value}"
    while len(out) < width:
        out += " "
    return out


with open(sys.argv[1], encoding="utf-8") as f:
    records = json.load(f)
lines = ["id     | name         | score | team     | tags"]
for record in records:
    if record["active"] and record["score"] >= 500:
        row = (f"{pad(record['id'], 6)} | {pad(record['name'], 12)} | {pad(record['score'], 5)} | "
               f"{pad(record['owner']['team'], 8)} | {','.join(record['tags'])}")
        lines.append(row)
table = "\n".join(lines)
print(f"{len(lines)} {len(table)}")
