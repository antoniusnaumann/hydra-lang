use env
use json

n := json::parse(env::args()[0])
s := ""
i := 0
while i < n
	s += "\(i),"
	i += 1
end

print len(s)
