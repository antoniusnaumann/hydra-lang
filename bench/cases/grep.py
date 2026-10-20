import sys

hits = 0
with open(sys.argv[1], encoding="utf-8") as f:
    for line in f:
        if "error" in line:
            hits += 1
print(hits)
