import sys

counts = {}
words = 0
best = 0
limit = int(sys.argv[2])
seen = 0
with open(sys.argv[1], encoding="utf-8") as f:
    for line in f:
        if seen == limit:
            break
        seen += 1
        for word in line.split():
            if word in counts:
                counts[word] += 1
            else:
                counts[word] = 1
            words += 1
            if counts[word] > best:
                best = counts[word]
print(f"{words} {len(counts)} {best}")
