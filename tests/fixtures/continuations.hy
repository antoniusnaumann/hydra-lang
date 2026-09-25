// Run: cargo run -- run tests/fixtures/continuations.hy
foo := { :bar : { :baz : 42 } }
a := 2

// Leading dots continue the same lookup chain: foo.bar.baz.
value := foo
.bar
.baz
print(value) // 42

// A leading minus continues subtraction, even without surrounding spaces.
difference := 32
-a
print(difference) // 30

// Normal precedence still applies: 10 - (2 * 3).
precedence := 10
-2
* 3
print(precedence) // 4

// Calls and indexing can each start on the following line.
identity := fn(items) items
selected := identity
([10, 20])
[1]
print(selected) // 20

commented := 32
// A comment-only line does not stop the subtraction.
-a
print(commented) // 30

// Trailing operators and split assignments also continue.
sum := 1 +
2
sum
+=
4
print(sum) // 7
