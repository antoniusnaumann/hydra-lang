import sys

n = int(sys.argv[1])
# A linear congruential generator, so both languages sort the same numbers.
seed = 1
xs = []
i = 0
while i < n:
    seed = seed * 48271 % 2147483647
    xs.append(seed)
    i += 1
s = sorted(xs)
print(f"{s[0]} {s[n // 2]} {s[n - 1]}")
