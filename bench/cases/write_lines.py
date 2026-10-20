import sys

n = int(sys.argv[1])
out = sys.argv[2]
lines = []
i = 0
while i < n:
    lines.append(f"line {i}: value {i * i % 9973}")
    i += 1
data = "\n".join(lines)
with open(out, "w", encoding="utf-8") as f:
    f.write(data)
with open(out, encoding="utf-8") as f:
    count = len(f.read().splitlines())
print(f"{len(data.encode())} {count}")
