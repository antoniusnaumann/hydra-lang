import sys

counts = {}
with open(sys.argv[1], encoding="utf-8") as f:
    for line in f:
        for word in line.split():
            if word in counts:
                counts[word] += 1
            else:
                counts[word] = 1

ranked = sorted((-n, word) for word, n in counts.items())
top = [f"{word}:{-n}" for n, word in ranked[:10]]
print(f"{len(counts)} {' '.join(top)}")
