use env
use json

n := json::parse(env::args()[0])
fn matrix(n, offset)
	rows := []
	i := 0
	while i < n
		row := []
		j := 0
		while j < n
			do push(&row, (i * n + j + offset) % 10)
			j += 1
		end

		do push(&rows, row)
		i += 1
	end

	return rows
end

a := matrix(n, 1)
b := matrix(n, 3)
c := []
i := 0
while i < n
	row := []
	j := 0
	while j < n
		acc := 0
		k := 0
		while k < n
			acc += a[i][k] * b[k][j]
			k += 1
		end

		do push(&row, acc)
		j += 1
	end

	do push(&c, row)
	i += 1
end

trace := 0
i = 0
while i < n
	trace += c[i][i]
	i += 1
end

print trace
