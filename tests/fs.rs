//! The `fs` module (`spec/hydra_fs.md`), and the two things it needed from the
//! language: a builtin module, and a qualified call that resolves by shape
//! among the module's own candidates (§7).

use std::path::PathBuf;

use hydra::value::to_text;
use hydra::vm::{run_source, Options, RunResult};

/// A directory of its own per test, so the ones that write cannot collide.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!("hydra-fs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("sandbox");
        Sandbox { root }
    }

    fn file(&self, name: &str, text: &str) -> &Sandbox {
        std::fs::write(self.root.join(name), text).expect("fixture");
        self
    }

    /// Run with the sandbox as the working directory, which is what the
    /// relative paths in these programs are relative to.
    fn run(&self, src: &str) -> RunResult {
        // `cwd()` is process-wide, so the tests that read it run one at a time.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _held = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&self.root).expect("enter the sandbox");
        let result = run_source(
            src,
            "t.hy",
            Options { search_path: Vec::new(), threads: 1, step_budget: 1, ..Options::default() },
        )
        .expect("compiles");
        std::env::set_current_dir(before).expect("leave the sandbox");
        result
    }

    fn eval(&self, src: &str, name: &str) -> String {
        let result = self.run(src);
        if let Some(crash) = &result.crash {
            panic!("unexpected crash: {crash}");
        }
        to_text(&result.root_scope.lookup(name).unwrap_or_else(|| panic!("no `{name}`")).read().unwrap().clone())
    }

    fn crash_of(&self, src: &str) -> String {
        self.run(src).crash.map(|c| c.message).unwrap_or_else(|| "<no crash>".into())
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

// --- the module itself (§7) --------------------------------------------------

#[test]
fn fs_is_a_builtin_module_and_a_plain_use_keeps_it_qualified() {
    let fs = Sandbox::new("builtin");
    assert_eq!(fs.eval("use fs\nx := fs::name(\"a/b.txt\")\n", "x"), "b.txt");
    // Qualified only, like any other module (§7).
    let crash = fs.crash_of("use fs\nx := name(\"a/b.txt\")\n");
    assert!(crash.contains("`name` is not declared"), "{crash}");
    // And there is no file to find, so this is the builtin answering.
    assert_eq!(fs.eval("use fs as *\nx := name(\"a/b.txt\")\n", "x"), "b.txt");
    assert_eq!(fs.eval("use fs as disk\nx := disk::stem(\"a/b.txt\")\n", "x"), "b");
}

#[test]
fn a_qualified_call_picks_among_the_modules_own_candidates() {
    // The whole reason a reader is two overloads rather than one with a
    // sentinel: `read(path)` crashes and `read(path, fallback)` does not.
    let fs = Sandbox::new("overload");
    fs.file("there.txt", "here\n");
    assert_eq!(fs.eval("use fs\nx := fs::read(\"there.txt\")\n", "x"), "here\n");
    assert_eq!(fs.eval("use fs\nx := fs::read(\"gone.txt\", \"(none)\")\n", "x"), "(none)");
    let crash = fs.crash_of("use fs\nx := fs::read(\"gone.txt\")\n");
    assert!(crash.contains("cannot read gone.txt"), "{crash}");
}

#[test]
fn a_reader_that_fell_back_says_why() {
    let fs = Sandbox::new("why");
    let src = "
use fs
text, why := fs::read(\"gone.txt\", \"\")
size, also := fs::size(\"gone.txt\", 0)
";
    assert_eq!(fs.eval(src, "why"), ":not_found");
    assert_eq!(fs.eval(src, "also"), ":not_found");
    // On the way through, the reason is `:null`.
    fs.file("there.txt", "x");
    assert_eq!(fs.eval("use fs\n_, why := fs::read(\"there.txt\", \"\")\n", "why"), ":null");
}

#[test]
fn the_qualifier_reaches_through_the_dot() {
    let fs = Sandbox::new("dot");
    fs.file("note.txt", "hello");
    let src = "
use fs
size := \"note.txt\".fs::size()
stem := \"a/b.txt\".fs::stem()
";
    assert_eq!(fs.eval(src, "size"), "5");
    assert_eq!(fs.eval(src, "stem"), "b");
}

// --- paths (§2) ---------------------------------------------------------------

#[test]
fn the_path_functions_are_pure_and_normalise() {
    let fs = Sandbox::new("paths");
    let src = "
use fs as *
joined := join(\"src\", \"vm\", \"mod.hy\")
slashes := join(\"a/\", \"/b\")
up := parent(\"src/vm/mod.hy\")
base := name(\"src/vm/mod.hy\")
short := stem(\"src/vm/mod.hy\")
kind := extension(\"src/vm/mod.hy\")
alone := parent(\"mod.hy\")
";
    assert_eq!(fs.eval(src, "joined"), "src/vm/mod.hy");
    assert_eq!(fs.eval(src, "slashes"), "a/b");
    assert_eq!(fs.eval(src, "up"), "src/vm");
    assert_eq!(fs.eval(src, "base"), "mod.hy");
    assert_eq!(fs.eval(src, "short"), "mod");
    assert_eq!(fs.eval(src, "kind"), "hy");
    assert_eq!(fs.eval(src, "alone"), ".");
}

// --- writing (§5) -------------------------------------------------------------

#[test]
fn writing_makes_the_parents_and_answers_the_path() {
    let fs = Sandbox::new("write");
    let src = "
use fs as *
where, bytes := write(\"out/deep/note.txt\", \"hello\\n\")
back := read(where)
";
    assert_eq!(fs.eval(src, "where"), "out/deep/note.txt");
    assert_eq!(fs.eval(src, "bytes"), "6");
    assert_eq!(fs.eval(src, "back"), "hello\n");
}

#[test]
fn append_adds_and_new_refuses() {
    let fs = Sandbox::new("modes");
    let src = "
use fs as *
write(\"log.txt\", \"one\\n\")
write(\"log.txt\", \"two\\n\", mode = :append)
both := lines(\"log.txt\")
";
    assert_eq!(fs.eval(src, "both"), "[one, two]");
    let crash = fs.crash_of("use fs as *\nwrite(\"a.txt\", \"x\")\nwrite(\"a.txt\", \"y\", mode = :new)\n");
    assert!(crash.contains("cannot write a.txt"), "{crash}");
}

#[test]
fn removing_answers_whether_there_was_anything_there() {
    let fs = Sandbox::new("remove");
    fs.file("gone.txt", "x");
    let src = "
use fs as *
first := remove(\"gone.txt\")
again := remove(\"gone.txt\")
";
    assert_eq!(fs.eval(src, "first"), ":true");
    assert_eq!(fs.eval(src, "again"), ":false");

    // A tree needs saying so.
    let src = "
use fs as *
write(\"tree/a/b.txt\", \"x\")
refused := is_dir(\"tree\")
";
    assert_eq!(fs.eval(src, "refused"), ":true");
    let crash = fs.crash_of("use fs as *\nremove(\"tree\")\n");
    assert!(crash.contains("recursive = :true"), "{crash}");
    assert_eq!(fs.eval("use fs as *\nx := remove(\"tree\", recursive = :true)\n", "x"), ":true");
}

#[test]
fn a_move_refuses_to_clobber_and_a_copy_does_not() {
    let fs = Sandbox::new("moves");
    fs.file("a.txt", "first").file("b.txt", "second");
    let crash = fs.crash_of("use fs as *\nmove(\"a.txt\", \"b.txt\")\n");
    assert!(crash.contains("already there"), "{crash}");
    // A copy that clobbers loses a copy; a move that clobbers loses the only
    // one (fs §5).
    assert_eq!(fs.eval("use fs as *\nx := copy(\"a.txt\", \"b.txt\")\n", "x"), "b.txt");
    assert_eq!(fs.eval("use fs as *\nx := read(\"b.txt\")\n", "x"), "first");
}

// --- reading a directory (§4) ---------------------------------------------------

#[test]
fn list_answers_full_paths_sorted_and_filtered() {
    let fs = Sandbox::new("list");
    let src = "
use fs as *
write(\"src/b.hy\", \"b\")
write(\"src/a.hy\", \"a\")
write(\"src/notes.txt\", \"n\")
write(\"src/deep/c.hy\", \"c\")
all := list(\"src\")
only := list(\"src\", match = \"*.hy\")
deep := list(\"src\", match = \"*.hy\", recursive = :true)
";
    assert_eq!(fs.eval(src, "all"), "[src/a.hy, src/b.hy, src/deep, src/notes.txt]");
    assert_eq!(fs.eval(src, "only"), "[src/a.hy, src/b.hy]");
    assert_eq!(fs.eval(src, "deep"), "[src/a.hy, src/b.hy, src/deep/c.hy]");
}

#[test]
fn a_flag_is_keyword_only_which_is_what_keeps_the_overloads_apart() {
    // A second positional can only be the fallback, never `match` (fs §4).
    let fs = Sandbox::new("flags");
    assert_eq!(fs.eval("use fs\nx := fs::list(\"nope\", [])\n", "x"), "[]");
    let crash = fs.crash_of("use fs\nx := fs::list(\"nope\")\n");
    assert!(crash.contains("cannot list nope"), "{crash}");
}

// --- the dot, and trails ---------------------------------------------------

#[test]
fn a_path_is_a_string_so_the_dot_reads_a_pipeline() {
    let fs = Sandbox::new("pipeline");
    let src = "
use fs as *
write(\"src/a.hy\", \"aa\")
write(\"src/b.hy\", \"bbb\")
total := 0
for entry in list(\"src\", match = \"*.hy\")
	total += entry.read().len()
end
names := []
for entry in list(\"src\", match = \"*.hy\")
	push(&names, entry.name())
end
";
    assert_eq!(fs.eval(src, "total"), "5");
    assert_eq!(fs.eval(src, "names"), "[a.hy, b.hy]");
}

#[test]
fn a_trail_may_read_a_file() {
    // I/O is a scheduling point, so a `parallel for` over a directory really
    // does overlap (fs §7.8).
    let fs = Sandbox::new("trails");
    let src = "
use fs as *
write(\"src/a.hy\", \"aa\")
write(\"src/b.hy\", \"bbb\")
seen := []
parallel for entry in list(\"src\", match = \"*.hy\")
	push(&seen, entry.read().len())
end
total := seen.len()
";
    assert_eq!(fs.eval(src, "total"), "2");
}
