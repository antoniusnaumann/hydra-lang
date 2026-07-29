i := 0
a := 50

while i < 10000000
	if i % 2 == 0
		a *= 50
		a -= i
	else
		a /= 51
	end
	i += 1
end

print(a)
