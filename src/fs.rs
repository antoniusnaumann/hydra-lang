//! The `fs` module (`spec/hydra_fs.md`).
//!
//! Three rules shape every signature here, and there is nothing else to
//! remember:
//!
//! * **Defaults absorb the ordinary failures.** Writing to a directory that
//!   does not exist creates it; making one that already exists is fine;
//!   removing what is not there answers `:false`.
//! * **Anything left crashes.** A denied permission or a file that is not there
//!   when you asked to read it ends the program, because there are no
//!   exceptions (§8) and a silent `:null` reaching the next line is how a
//!   script destroys data.
//! * **A reader opts out with a `fallback`, and then says why.** That is a
//!   second overload rather than a sentinel: `read(path)` crashes and
//!   `read(path, fallback)` does not, and resolution by shape picks between
//!   them exactly as it does for two functions a program declares (§3).

use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::errors::Crash;
use crate::value::{boolean, deref, list_items, new_list, sym, to_text, Native, Value};

/// The closed set of reasons a call can fail (fs §7.6). The symbol is what a
/// program branches on; the message is where the operating system's own words
/// go.
fn reason(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::NotFound => "not_found",
        std::io::ErrorKind::PermissionDenied => "denied",
        std::io::ErrorKind::AlreadyExists => "exists",
        std::io::ErrorKind::IsADirectory => "is_dir",
        std::io::ErrorKind::NotADirectory => "not_dir",
        std::io::ErrorKind::InvalidData => "encoding",
        _ => "io",
    }
}

/// A failed call either crashes or takes the fallback, and the two overloads
/// are the only difference between them.
enum Failed {
    Crash(String, &'static str),
}

impl Failed {
    fn at(what: &str, path: &str, error: &std::io::Error) -> Failed {
        Failed::Crash(format!("cannot {what} {path}: {error}"), reason(error))
    }

    /// With no fallback the program ends; with one it is the answer, and the
    /// reason comes back beside it (fs §1).
    fn answer(self, fallback: Option<Value>) -> Result<Vec<Value>, Crash> {
        let Failed::Crash(message, why) = self;
        match fallback {
            Some(value) => Ok(vec![value, Value::Sym(sym(why))]),
            None => Err(Crash::new(message)),
        }
    }
}

fn text_of(value: &Value) -> Result<String, Crash> {
    Ok(to_text(&deref(value)?))
}

fn ok(value: Value) -> Result<Vec<Value>, Crash> {
    Ok(vec![value])
}

/// A reader's answer on the way out: the value, and `:null` for "nothing went
/// wrong". The second is dropped in silence where a binding does not name it
/// (channels §6.2).
fn ok_with_reason(value: Value) -> Result<Vec<Value>, Crash> {
    Ok(vec![value, Value::null()])
}

fn string(text: impl Into<String>) -> Value {
    Value::Str(std::sync::Arc::from(text.into().as_str()))
}

/// Normalise a joined path: `join("a/", "/b")` is `"a/b"`, and `.` and a
/// trailing separator go away. `..` stays, because resolving it without asking
/// the filesystem would lie about symlinks.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

fn show(path: &Path) -> Value {
    string(path.to_string_lossy().to_string())
}

/// `*` and `?`, and nothing else — `recursive` is what `**` would have been
/// (fs §4).
fn matches(pattern: &str, name: &str) -> bool {
    fn walk(pattern: &[char], name: &[char]) -> bool {
        match pattern.first() {
            None => name.is_empty(),
            Some('*') => {
                walk(&pattern[1..], name)
                    || (!name.is_empty() && walk(pattern, &name[1..]))
            }
            Some('?') => !name.is_empty() && walk(&pattern[1..], &name[1..]),
            Some(c) => name.first() == Some(c) && walk(&pattern[1..], &name[1..]),
        }
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    walk(&pattern, &name)
}

fn entries(dir: &Path, pattern: &str, recursive: bool, into: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect();
    // Sorted, so a program over a directory is reproducible (fs §4).
    found.sort();
    for path in found {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if matches(pattern, &name) {
            into.push(path.clone());
        }
        if recursive && path.is_dir() {
            entries(&path, pattern, recursive, into)?;
        }
    }
    Ok(())
}

fn write_mode(value: Option<Value>) -> Result<&'static str, Crash> {
    let Some(value) = value else { return Ok("replace") };
    match deref(&value)? {
        Value::Sym(s) if s == sym("replace") => Ok("replace"),
        Value::Sym(s) if s == sym("append") => Ok("append"),
        Value::Sym(s) if s == sym("new") => Ok("new"),
        other => Err(Crash::new(format!(
            "`write`'s mode is .replace, .append or .new, got {}",
            to_text(&other)
        ))),
    }
}

fn truthy(value: Option<Value>, default: bool) -> Result<bool, Crash> {
    match value {
        None => Ok(default),
        Some(value) => Ok(deref(&value)?.truthy()),
    }
}

/// Make the parents of a path, which is what `parents = :true` asks for.
fn make_parents(path: &Path) -> std::io::Result<()> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => std::fs::create_dir_all(parent),
        _ => Ok(()),
    }
}

/// Run one of the `fs` builtins. `args` is already matched to its parameters,
/// so a missing slot is a default this call did not fill.
pub fn call(native: Native, args: &[Option<Value>]) -> Result<Vec<Value>, Crash> {
    let arg = |i: usize| args.get(i).cloned().flatten();
    let need = |i: usize| -> Result<String, Crash> {
        text_of(&arg(i).unwrap_or_else(Value::null))
    };

    match native {
        // --- paths: no I/O, so nothing here can fail ------------------------
        Native::FsJoin => {
            let mut path = PathBuf::from(need(0)?);
            let parts = match deref(&arg(1).unwrap_or_else(|| new_list(Vec::new())))? {
                Value::List(list) => list_items(&list),
                other => vec![other],
            };
            for part in parts {
                let part = text_of(&part)?;
                // A leading separator on a part joins rather than replaces:
                // `join("a/", "/b")` is `"a/b"`.
                path.push(part.trim_start_matches('/'));
            }
            ok(show(&normalise(&path)))
        }
        Native::FsParent => {
            let path = PathBuf::from(need(0)?);
            let parent = path.parent().unwrap_or(Path::new(""));
            ok(show(if parent.as_os_str().is_empty() { Path::new(".") } else { parent }))
        }
        Native::FsName => {
            let path = PathBuf::from(need(0)?);
            ok(string(path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()))
        }
        Native::FsStem => {
            let path = PathBuf::from(need(0)?);
            ok(string(path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()))
        }
        Native::FsExtension => {
            let path = PathBuf::from(need(0)?);
            ok(string(path.extension().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()))
        }
        Native::FsAbsolute => {
            let path = PathBuf::from(need(0)?);
            if path.is_absolute() {
                return ok(show(&normalise(&path)));
            }
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            ok(show(&normalise(&cwd.join(path))))
        }

        // --- asking: a path that cannot be looked at is not there -----------
        Native::FsExists => ok(boolean(Path::new(&need(0)?).exists())),
        Native::FsIsFile => ok(boolean(Path::new(&need(0)?).is_file())),
        Native::FsIsDir => ok(boolean(Path::new(&need(0)?).is_dir())),

        Native::FsSize | Native::FsSizeOr => {
            let path = need(0)?;
            let fallback = if native == Native::FsSizeOr { arg(1) } else { None };
            match std::fs::metadata(&path) {
                Ok(meta) => ok_with_reason(Value::Num(meta.len() as f64)),
                Err(e) => Failed::at("measure", &path, &e).answer(fallback),
            }
        }
        Native::FsModified | Native::FsModifiedOr => {
            let path = need(0)?;
            let fallback = if native == Native::FsModifiedOr { arg(1) } else { None };
            let seconds = std::fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .map(|at| at.duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0));
            match seconds {
                Ok(seconds) => ok_with_reason(Value::Num(seconds)),
                Err(e) => Failed::at("read the age of", &path, &e).answer(fallback),
            }
        }

        // --- reading --------------------------------------------------------
        Native::FsRead | Native::FsReadOr => {
            let path = need(0)?;
            let fallback = if native == Native::FsReadOr { arg(1) } else { None };
            match std::fs::read_to_string(&path) {
                Ok(text) => ok_with_reason(string(text)),
                Err(e) => Failed::at("read", &path, &e).answer(fallback),
            }
        }
        Native::FsLines | Native::FsLinesOr => {
            let path = need(0)?;
            let fallback = if native == Native::FsLinesOr { arg(1) } else { None };
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    let lines: Vec<Value> = text.lines().map(string).collect();
                    ok_with_reason(new_list(lines))
                }
                Err(e) => Failed::at("read", &path, &e).answer(fallback),
            }
        }
        Native::FsList | Native::FsListOr => {
            let dir = need(0)?;
            let (fallback, flags) = match native {
                Native::FsListOr => (arg(1), 3),
                _ => (None, 2),
            };
            let pattern = match arg(flags) {
                Some(value) => text_of(&value)?,
                None => "*".to_string(),
            };
            let recursive = truthy(arg(flags + 1), false)?;
            let mut found: Vec<PathBuf> = Vec::new();
            match entries(Path::new(&dir), &pattern, recursive, &mut found) {
                // Full paths, joined onto `dir`, because the next thing anyone
                // does with an entry is read it (fs §4).
                Ok(()) => ok_with_reason(new_list(found.iter().map(|p| show(p)).collect())),
                Err(e) => Failed::at("list", &dir, &e).answer(fallback),
            }
        }

        // --- writing --------------------------------------------------------
        Native::FsWrite => {
            let path = need(0)?;
            let text = need(1)?;
            let mode = write_mode(arg(3))?;
            if truthy(arg(4), true)? {
                make_parents(Path::new(&path))
                    .map_err(|e| Crash::new(format!("cannot make the parents of {path}: {e}")))?;
            }
            let written = match mode {
                "append" => std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .and_then(|mut file| std::io::Write::write_all(&mut file, text.as_bytes())),
                "new" => std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&path)
                    .and_then(|mut file| std::io::Write::write_all(&mut file, text.as_bytes())),
                _ => std::fs::write(&path, text.as_bytes()),
            };
            match written {
                Ok(()) => Ok(vec![string(path), Value::Num(text.len() as f64)]),
                Err(e) => Err(Crash::new(format!("cannot write {path}: {e}"))),
            }
        }
        Native::FsCopy | Native::FsMove => {
            let source = need(0)?;
            let target = need(1)?;
            let moving = native == Native::FsMove;
            // A copy that clobbers loses a copy; a move that clobbers loses the
            // only one (fs §5).
            let overwrite = truthy(arg(3), !moving)?;
            if !overwrite && Path::new(&target).exists() {
                return Err(Crash::new(format!(
                    "{target} is already there: pass `overwrite = :true` to replace it"
                )));
            }
            if truthy(arg(4), true)? {
                make_parents(Path::new(&target))
                    .map_err(|e| Crash::new(format!("cannot make the parents of {target}: {e}")))?;
            }
            let done = if moving {
                std::fs::rename(&source, &target)
            } else if Path::new(&source).is_dir() {
                copy_tree(Path::new(&source), Path::new(&target))
            } else {
                std::fs::copy(&source, &target).map(|_| ())
            };
            match done {
                Ok(()) => ok(string(target)),
                Err(e) => Err(Crash::new(format!(
                    "cannot {} {source} to {target}: {e}",
                    if moving { "move" } else { "copy" }
                ))),
            }
        }
        Native::FsRemove => {
            let path = need(0)?;
            let recursive = truthy(arg(2), false)?;
            let at = Path::new(&path);
            // Removing what is not there is not a failure — it answers whether
            // there was anything to remove (fs §5).
            if !at.exists() {
                return ok(boolean(false));
            }
            let done = if at.is_dir() {
                if recursive {
                    std::fs::remove_dir_all(at)
                } else {
                    std::fs::remove_dir(at)
                }
            } else {
                std::fs::remove_file(at)
            };
            match done {
                Ok(()) => ok(boolean(true)),
                Err(e) if at.is_dir() && !recursive => Err(Crash::new(format!(
                    "cannot remove the directory {path}: {e}; \
                     pass `recursive = :true` to remove a tree"
                ))),
                Err(e) => Err(Crash::new(format!("cannot remove {path}: {e}"))),
            }
        }
        Native::FsMakeDir => {
            let path = need(0)?;
            let parents = truthy(arg(2), true)?;
            let at = Path::new(&path);
            // Making one that is already there is fine (fs §1).
            if at.is_dir() {
                return ok(string(path));
            }
            let done =
                if parents { std::fs::create_dir_all(at) } else { std::fs::create_dir(at) };
            match done {
                Ok(()) => ok(string(path)),
                Err(e) => Err(Crash::new(format!("cannot make the directory {path}: {e}"))),
            }
        }

        // --- where the process is -------------------------------------------
        Native::FsCwd => match std::env::current_dir() {
            Ok(path) => ok(show(&path)),
            Err(e) => Err(Crash::new(format!("cannot read the working directory: {e}"))),
        },
        Native::FsHome => match home_dir() {
            Some(path) => ok(show(&path)),
            None => Err(Crash::new("there is no home directory in this environment")),
        },
        Native::FsTemp => ok(show(&std::env::temp_dir())),

        other => unreachable!("{other:?} is not an fs builtin"),
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// A copy of a directory copies its tree — one name for both, since the
/// difference is the receiver's and not the caller's (fs §5).
fn copy_tree(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let to = target.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}
