import sys

config = {}
sections = []
current = ""
keys = 0
with open(sys.argv[1], encoding="utf-8") as f:
    for raw in f:
        line = raw.strip()
        if line == "" or line.startswith(";") or line.startswith("#"):
            continue
        if line.startswith("["):
            current = line.replace("[", "").replace("]", "")
            config[current] = {}
            sections.append(current)
            continue
        parts = line.split("=")
        key = parts[0].strip()
        value = "=".join(parts[1:]).strip()
        config[current][key] = value
        keys += 1

ports = 0
enabled = 0
timeouts = 0
for name in sections:
    section = config[name]
    ports += int(section["port"])
    if section["enabled"] == "true":
        enabled += 1
    timeouts += int(section["timeout"])
print(f"{len(sections)} {keys} {ports} {enabled} {timeouts}")
