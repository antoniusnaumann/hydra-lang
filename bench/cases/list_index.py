import sys

n = int(sys.argv[1])
xs = list(range(n))
i = 0
while i < n:
    xs[i] = xs[i] * 3 % 11
    i += 1
total = 0
i = 0
while i < n:
    total += xs[i]
    i += 1
print(total)
