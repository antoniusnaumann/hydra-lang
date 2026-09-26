// Eager pipelines produce ordinary lists at every step.
use list

squares := list::range(8)
.list::map(fn(n) n * n)
.list::filter(fn(n) n > 10)
print squares

// Partition and unzip return two values.
even, odd := list::partition(squares, fn(n) n % 2 == 0)
print even
print odd

// Consecutive groups, overlapping windows, and combinations.
print list::chunk_by([1, 3, 2, 4, 5], fn(n) n % 2)
print list::windows(squares, 2)
print list::combinations(["red", "green", "blue"], 2)
