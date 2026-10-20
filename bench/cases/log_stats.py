import sys

statuses = {}
paths = {}
requests = 0
with open(sys.argv[1], encoding="utf-8") as f:
    for line in f:
        f = line.rstrip("\n").split(" ")
        status = f[4]
        if status in statuses:
            statuses[status] += 1
        else:
            statuses[status] = 1
        path = f[3]
        nbytes = int(f[5])
        ms = int(f[6])
        if path in paths:
            stats = paths[path]
            stats["hits"] += 1
            stats["bytes"] += nbytes
            if ms > stats["slowest"]:
                stats["slowest"] = ms
        else:
            paths[path] = {"hits": 1, "bytes": nbytes, "slowest": ms}
        requests += 1

counts = [f"{name}={statuses[name]}" for name in sorted(statuses)]
heaviest = ""
most = -1
slow = 0
for name in sorted(paths):
    stats = paths[name]
    if stats["bytes"] > most:
        most = stats["bytes"]
        heaviest = name
    slow += stats["slowest"]
print(f"{requests} {','.join(counts)} {heaviest} {most} {slow}")
