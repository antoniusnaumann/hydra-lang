import sys

with open(sys.argv[1], encoding="utf-8") as f:
    lines = f.read().splitlines()
header = lines[0].split(" ")
n = int(header[0])
adjacent = [[] for _ in range(n)]
i = 1
while i < len(lines):
    edge = lines[i].split(" ")
    a = int(edge[0])
    b = int(edge[1])
    adjacent[a].append(b)
    adjacent[b].append(a)
    i += 1

distance = [-1] * n
distance[0] = 0
queue = [0]
head = 0
while head < len(queue):
    node = queue[head]
    head += 1
    for nxt in adjacent[node]:
        if distance[nxt] == -1:
            distance[nxt] = distance[node] + 1
            queue.append(nxt)

farthest = 0
total = 0
for d in distance:
    if d > farthest:
        farthest = d
    if d > 0:
        total += d
print(f"{n} {len(queue)} {farthest} {total}")
