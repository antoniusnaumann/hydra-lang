// Callback operations run as ordinary VM frames, so callbacks can suspend.
fn map(items, f)
	out := []
	for item in items
		_ =::push(&out, f(item))
	end

	::return(out)
end

fn filter(items, f)
	out := []
	for item in items
		if f(item)
			_ =::push(&out, item)
		end
	end

	::return(out)
end

fn filter_map(items, f)
	out := []
	for item in items
		value := f(item)
		if value != :null
			_ =::push(&out, value)
		end
	end

	::return(out)
end

fn flat_map(items, f)

	::return(flatten(map(items, f)))
end

fn take_while(items, f)
	out := []
	for item in items
		if not f(item)

			::return(out)
		end

		_ =::push(&out, item)
	end

	::return(out)
end

fn skip_while(items, f)
	index := 0
	for item in items
		if not f(item)

			::return(skip(items, index))
		end

		index += 1
	end

	::return([])
end

fn map_while(items, f)
	out := []
	for item in items
		value := f(item)
		if value == :null

			::return(out)
		end

		_ =::push(&out, value)
	end

	::return(out)
end

fn for_each(items, f)
	for item in items
		_ = f(item)
	end
end

fn inspect(items, f)
	_ = for_each(items, f)

	::return(items)
end

fn any(items, f)
	for item in items
		if f(item)

			::return(:true)
		end
	end

	::return(:false)
end

fn all(items, f)
	for item in items
		if not f(item)

			::return(:false)
		end
	end

	::return(:true)
end

fn find(items, f)
	for item in items
		if f(item)

			::return(item)
		end
	end

	::return(:null)
end

fn find_map(items, f)
	for item in items
		value := f(item)
		if value != :null

			::return(value)
		end
	end

	::return(:null)
end

fn position(items, f)
	index := 0
	for item in items
		if f(item)

			::return(index)
		end

		index += 1
	end

	::return(:null)
end

fn rposition(items, f)
	index :=::len(items)
	for item in rev(items)
		index -= 1
		if f(item)

			::return(index)
		end
	end

	::return(:null)
end

fn partition(items, f)
	yes := []
	no := []
	for item in items
		if f(item)
			_ =::push(&yes, item)
		else
			_ =::push(&no, item)
		end
	end

	::return(yes, no)
end

fn fold(items, initial, f)
	accumulator := initial
	for item in items
		accumulator = f(accumulator, item)
	end

	::return(accumulator)
end

fn reduce(items, f)
	if::len(items) == 0

		::return(:null)
	end

	::return(fold(skip(items, 1), items[0], f))
end

fn unique_by(items, f)
	keys := []
	out := []
	for item in items
		key := f(item)
		if not any(keys, fn(k) k == key)
			_ =::push(&keys, key)
			_ =::push(&out, item)
		end
	end

	::return(out)
end

fn dedup_by_key(items, f)
	keys := []
	out := []
	for item in items
		key := f(item)
		if::len(keys) == 0 or keys[-1] != key
			_ =::push(&out, item)
		end

		_ =::push(&keys, key)
	end

	::return(out)
end

fn group_by(items, f)
	groups := []
	for item in items
		key := f(item)
		index := position(groups, fn(group) group[0] == key)
		if index == :null
			_ =::push(&groups, [key, [item]])
		else
			_ =::push(&groups[index][1], item)
		end
	end

	::return(groups)
end

fn chunk_by(items, f)
	groups := []
	for item in items
		key := f(item)
		if::len(groups) == 0 or groups[-1][0] != key
			_ =::push(&groups, [key, [item]])
		else
			_ =::push(&groups[-1][1], item)
		end
	end

	::return(groups)
end

fn sorted_by_key(items, f)
	keyed := []
	index := 0
	for item in items
		_ =::push(&keyed, [f(item), index])
		index += 1
	end

	::return(map(sorted(keyed), fn(pair) items[pair[1]]))
end

fn min_by_key(items, f)
	if::len(items) == 0

		::return(:null)
	end

	best := items[0]
	key := f(best)
	_ = sorted([key])
	for item in skip(items, 1)
		candidate := f(item)
		order := sorted([[key, 0], [candidate, 1]])
		if order[0][1] == 1
			best = item
			key = candidate
		end
	end

	::return(best)
end

fn max_by_key(items, f)
	if::len(items) == 0

		::return(:null)
	end

	best := items[0]
	key := f(best)
	_ = sorted([key])
	for item in skip(items, 1)
		candidate := f(item)
		order := sorted([[key, 0], [candidate, 1]])
		if order[-1][1] == 1
			best = item
			key = candidate
		end
	end

	::return(best)
end

fn sorted_by(items, compare)
	if::len(items) < 2

		::return(items)
	end

	middle :=::len(items) >>> 1
	left := sorted_by(take(items, middle), compare)
	right := sorted_by(skip(items, middle), compare)
	out := []
	a := 0
	b := 0
	while a <::len(left) and b <::len(right)
		if compare(left[a], right[b]) <= 0
			_ =::push(&out, left[a])
			a += 1
		else
			_ =::push(&out, right[b])
			b += 1
		end
	end

	::return(chain(out, chain(skip(left, a), skip(right, b))))
end

fn dedup_by(items, f)
	out := []
	for item in items
		if::len(out) == 0 or not f(out[-1], item)
			_ =::push(&out, item)
		end
	end

	::return(out)
end

fn min_by(items, compare)
	if::len(items) == 0

		::return(:null)
	end

	best := items[0]
	for item in skip(items, 1)
		if compare(item, best) < 0
			best = item
		end
	end

	::return(best)
end

fn max_by(items, compare)
	if::len(items) == 0

		::return(:null)
	end

	best := items[0]
	for item in skip(items, 1)
		if compare(item, best) >= 0
			best = item
		end
	end

	::return(best)
end
