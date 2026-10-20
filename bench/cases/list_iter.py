import sys

n = int(sys.argv[1])
xs = list(range(n))
total = 0
for x in xs:
    if x % 3 == 0:
        total += x
print(total)
