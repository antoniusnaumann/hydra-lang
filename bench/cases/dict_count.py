import sys

n = int(sys.argv[1])
counts = {}
i = 0
while i < n:
    key = f"k{i * 7919 % 1000}"
    if key in counts:
        counts[key] += 1
    else:
        counts[key] = 1
    i += 1
total = 0
j = 0
while j < 1000:
    total += counts[f"k{j}"]
    j += 1
print(f"{len(counts)} {total} {counts['k0']}")
