import sys


def apply(item, kind, qty):
    if kind == "restock":
        item["stock"] += qty
    elif kind == "order":
        if item["stock"] >= qty:
            item["stock"] -= qty
            item["sold"] += qty
        else:
            item["backordered"] += qty
    else:
        back = qty
        if back > item["sold"]:
            back = item["sold"]
        item["sold"] -= back
        item["stock"] += back


items = {}
with open(sys.argv[1], encoding="utf-8") as f:
    next(f)
    for line in f:
        f = line.rstrip("\n").split(",")
        key = f[1]
        if key not in items:
            items[key] = {"stock": 0, "sold": 0, "backordered": 0}
        apply(items[key], f[0], int(f[2]))

stock = sold = backordered = empty = 0
for item in items.values():
    stock += item["stock"]
    sold += item["sold"]
    backordered += item["backordered"]
    if item["stock"] == 0:
        empty += 1
print(f"{len(items)} {stock} {sold} {backordered} {empty}")
