import sys

n = int(sys.argv[1])
squares = [x * x % 1000 for x in range(n)]
print(sum([x for x in squares if x % 2 == 0]))
