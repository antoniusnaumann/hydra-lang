// files.hy — the `fs` module (spec/hydra_fs.md), which is built in: there is no
// file to find, and `use` brings it in for qualified calling.
//
//	hydra run examples/files.hy

use fs

// Paths are pure string functions, so nothing here touches a disk.
where := fs::join(fs::temp(), "hydra-files-example")
print("working in \(where)")

// Writing makes the parents it needs, and answers the path it wrote — with the
// byte count beside it, for whoever wants it (§1, §5).
note, bytes := fs::write(fs::join(where, "notes", "first.txt"), "one\n")
print("wrote \(bytes) bytes to \(note)")

fs::write(note, "two\n", mode = .append)
print(fs::lines(note))

// A reader is two functions: one that crashes when the file is not there, and
// one that takes a fallback and says why it had to (§1).
missing, why := fs::read(fs::join(where, "gone.txt"), "(nothing)")
print("\(missing) — because \(why)")

// A path is a string, so the dot reaches every one of these, and a qualifier
// reaches through the dot (§5.2, §7).
print("\(note.fs::name()) is \(note.fs::size()) bytes, and a \(note.fs::extension()) file")

// `list` answers with full paths, sorted, so a program over a directory is
// reproducible (§4).
fs::write(fs::join(where, "notes", "second.txt"), "three\n")
fs::write(fs::join(where, "notes", "ignore.md"), "not this one\n")

for entry in fs::list(fs::join(where, "notes"), match = "*.txt")
	print("  \(entry.fs::name()): \(entry.fs::size()) bytes")
end

// I/O is a scheduling point, so a trail that reads a file lets its siblings
// run: this is real overlap, not a loop with extra steps (§7.8).
total := 0
parallel for entry in fs::list(fs::join(where, "notes"), match = "*.txt")
	total += entry.fs::read().len()
end
print("\(total) characters across the notes")

// Removing what is not there is not a failure — it answers whether there was
// anything to remove (§5).
print("cleaned up: \(fs::remove(where, recursive = .true))")
print("again: \(fs::remove(where, recursive = .true))")
