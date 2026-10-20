import sys

totals = {}
rows = 0
flagged = 0
with open(sys.argv[1], encoding="utf-8") as f:
    next(f)
    for line in f:
        fields = line.rstrip("\n").split(",")
        key = fields[1]
        amount = int(fields[2])
        if key in totals:
            totals[key] += amount
        else:
            totals[key] = amount
        if fields[3] == "1":
            flagged += amount
        rows += 1
print(f"{rows} {len(totals)} {totals['books']} {flagged}")
