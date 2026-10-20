use env
use fs
use json
use text

fn apply(&item, kind, qty)
	if kind == "restock"
		item.stock += qty
	else if kind == "order"
		if item.stock >= qty
			item.stock -= qty
			item.sold += qty
		else
			item.backordered += qty
		end

	else
		// A cancellation returns what was sold, up to the quantity.
		back := qty
		if back > item.sold
			back = item.sold
		end

		item.sold -= back
		item.stock += back
	end
end

items := {}
skus := []
first := :true
for line in fs::lines(env::args()[0])
	if first
		first = :false
		continue()
	end

	f := text::split(line, ",")
	key := :"\(f[1])"
	if not has(items, key)
		items[key] = { :stock : 0, :sold : 0, :backordered : 0 }
		do push(&skus, f[1])
	end

	apply(&items[key], f[0], json::parse(f[2]))
end

stock := 0
sold := 0
backordered := 0
empty := 0
for sku in skus
	item := items[:"\(sku)"]
	stock += item.stock
	sold += item.sold
	backordered += item.backordered
	if item.stock == 0
		empty += 1
	end
end

print "\(len(skus)) \(stock) \(sold) \(backordered) \(empty)"
