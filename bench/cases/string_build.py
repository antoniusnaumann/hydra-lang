import sys

n = int(sys.argv[1])
s = ""
i = 0
while i < n:
    s += f"{i},"
    i += 1
print(len(s))
