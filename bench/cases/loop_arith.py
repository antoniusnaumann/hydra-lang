import sys

n = int(sys.argv[1])
i = 0
total = 0
while i < n:
    total += i % 7 * 3 - i % 5
    i += 1
print(total)
