import sys

count = 0
chars = 0
longest = 0
with open(sys.argv[1], encoding="utf-8") as f:
    for line in f:
        count += 1
        size = len(line.rstrip("\n"))
        chars += size
        if size > longest:
            longest = size
print(f"{count} {chars} {longest}")
