// Run: printf ' one \n two \n' | hydra run examples/stdin.hy
use io
use text

line := io::read_line()
while line != :null
	print text::trim(line)
	line = io::read_line()
end
