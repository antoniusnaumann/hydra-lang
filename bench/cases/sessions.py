import sys

gap = 300
visits = []
with open(sys.argv[1], encoding="utf-8") as f:
    for line in f:
        f = line.rstrip("\n").split(" ")
        visits.append((f[1], int(f[0])))

ordered = sorted(visits)
sessions = 0
longest = 0
length = 0
client = ""
last = 0
for visit in ordered:
    if visit[0] != client or visit[1] - last > gap:
        sessions += 1
        length = 0
        client = visit[0]
    length += 1
    if length > longest:
        longest = length
    last = visit[1]
print(f"{len(ordered)} {sessions} {longest}")
