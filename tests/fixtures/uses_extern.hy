use json

body := read_file("x")
print(json::decode(body))
