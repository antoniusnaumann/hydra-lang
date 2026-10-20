use env
use fs
use list
use text

counts := {}
words := []
for line in fs::lines(env::args()[0])
	for word in text::split(line)
		key := :"\(word)"
		if has(counts, key)
			counts[key] += 1
		else
			counts[key] = 1
			do push(&words, word)
		end
	end
end

// Most frequent first, ties alphabetical: sorting [-count, word] pairs.
ranked := []
for word in words
	do push(&ranked, [0 - counts[:"\(word)"], word])
end

top := []
for pair in list::take(list::sorted(ranked), 10)
	do push(&top, "\(pair[1]):\(0 - pair[0])")
end

print "\(len(words)) \(text::join(top, " "))"
