// Run: cargo run -- run tests/fixtures/continuation_atoms.hy

// Hyphens and plus signs belong to this one atom.
key := :some-other-prop+interesting_added_info
print(key)

// Colons, commas and braces still delimit map entries.
record := { :some-other-prop+interesting_added_info : 17, :other : 9 }
print(record[key]) // 17

// Dots and trailing operator characters also belong to bare atoms.
atoms := [:a.b, :trailing-, :x*y]
print(atoms) // [:a.b, :trailing-, :x*y]

// A lookup still uses an identifier; its minus sign is subtraction.
counter := { :total : 10 }
value := counter
.total - 1
print(value) // 9

// Quote a lookup key when punctuation should belong to its name.
value = record
."some-other-prop+interesting_added_info"
print(value) // 17
