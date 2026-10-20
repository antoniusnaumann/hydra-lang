import sys

n = int(sys.argv[1])
xs = []
i = 0
while i < n:
    xs.append(i * 2)
    i += 1
print(f"{len(xs)} {xs[n - 1]}")
