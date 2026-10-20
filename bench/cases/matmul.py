import sys

n = int(sys.argv[1])


def matrix(n, offset):
    rows = []
    i = 0
    while i < n:
        row = []
        j = 0
        while j < n:
            row.append((i * n + j + offset) % 10)
            j += 1
        rows.append(row)
        i += 1
    return rows


a = matrix(n, 1)
b = matrix(n, 3)
c = []
i = 0
while i < n:
    row = []
    j = 0
    while j < n:
        acc = 0
        k = 0
        while k < n:
            acc += a[i][k] * b[k][j]
            k += 1
        row.append(acc)
        j += 1
    c.append(row)
    i += 1
trace = 0
i = 0
while i < n:
    trace += c[i][i]
    i += 1
print(trace)
