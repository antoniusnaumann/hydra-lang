fn dig(d)
	return d.missing
end

record := { :present : 1 }
value := dig(record)
