//! Values (spec §5): the copy-on-write containers, `&` references, interned
//! symbols, the two equalities, and the operators.
//!
//! ## Value semantics
//!
//! Every value behaves as a value: assignment, parameter passing, insertion
//! into a list or dict, and returning all copy deeply (§5.1). The copy is a
//! *semantic guarantee*, and copy-on-write is how it is paid for: a copy marks
//! the node `shared` and clones only the handle; the first write through a
//! shared handle clones the node and repoints along the path.
//!
//! `===` therefore compares the COW buffer, which is exactly the simplification
//! §5.1 signs off on: an untouched copy still reports identical to its source,
//! and the split becomes visible on the first write.
//!
//! ## References
//!
//! `&lvalue` is a *path*, not a pointer: the root variable's [`Cell`] plus the
//! keys and indices to walk from it. That is what lets a reference survive the
//! path copying happening underneath it, and it falls out of §5.1's rule that
//! only a variable, a dict key or a list element may be referenced.
//!
//! ## Threads
//!
//! Trails run on a pool of OS threads, so every value is shared with an `Arc`
//! and every node carries its own lock. That is an implementation detail and
//! not a change to §9.2: a lock per node and per binding is what makes
//! "concurrent writes are last-write-wins" true — one write wins whole, and
//! nothing is ever torn.
//!
//! Locks are taken **root to leaf and never re-entered**. A `&` that would send
//! the walk back to another root unwinds first (see `Walk`), so the one way to
//! build a cycle cannot deadlock.
//!
//! [`update_place`] is the one place that reads and writes under the same lock,
//! which is what makes `a += 1` atomic with respect to `a` where §9.2 leaves a
//! plain `a = a + 1` last-write-wins (QUESTIONS.md §20).

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock, Weak};

use crate::compile::{Chunk, ParamInfo};
use crate::errors::Crash;
use crate::scope::ScopeRef;

/// The storage a name refers to. `&` points at one of these plus a path.
pub type Cell = Arc<RwLock<Value>>;

pub fn cell(value: Value) -> Cell {
    Arc::new(RwLock::new(value))
}

/// How many `&` hops one read or write follows before giving up. Only a cycle
/// of references can exceed it.
const MAX_REDIRECTS: usize = 64;

// --- symbols ----------------------------------------------------------------

/// An interned tag such as `:null` (§2, §5).
///
/// Symbols are compared by identity. The intern table holds *weak* entries, so
/// symbols minted from input data can be collected again — without that, a
/// program that decodes data in a loop grows without bound (§2).
#[derive(Clone)]
pub struct Sym(Arc<str>);

impl Sym {
    pub fn name(&self) -> &str {
        &self.0
    }

    pub fn ptr(&self) -> *const u8 {
        Arc::as_ptr(&self.0) as *const u8
    }
}

impl PartialEq for Sym {
    fn eq(&self, other: &Sym) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Sym {}

impl std::hash::Hash for Sym {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.ptr().hash(state);
    }
}

impl fmt::Debug for Sym {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, ":{}", self.0)
    }
}

static INTERNED: LazyLock<Mutex<HashMap<Box<str>, Weak<str>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Intern a symbol name. Equal names always give the identical symbol.
pub fn sym(name: &str) -> Sym {
    let mut table = INTERNED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(weak) = table.get(name) {
        if let Some(strong) = weak.upgrade() {
            return Sym(strong);
        }
    }
    let strong: Arc<str> = Arc::from(name);
    table.insert(Box::from(name), Arc::downgrade(&strong));
    if table.len().is_power_of_two() {
        // Drop entries whose symbol has gone away. Amortised, so minting
        // symbols in a loop does not leave the table growing.
        table.retain(|_, weak| weak.strong_count() > 0);
    }
    Sym(strong)
}

/// How many names the intern table is holding, for tests about collectability.
pub fn interned_count() -> usize {
    let mut table = INTERNED.lock().unwrap_or_else(|e| e.into_inner());
    table.retain(|_, weak| weak.strong_count() > 0);
    table.len()
}

static SYM_NULL: LazyLock<Sym> = LazyLock::new(|| sym("null"));
static SYM_TRUE: LazyLock<Sym> = LazyLock::new(|| sym("true"));
static SYM_FALSE: LazyLock<Sym> = LazyLock::new(|| sym("false"));

pub fn sym_null() -> Sym {
    SYM_NULL.clone()
}

pub fn sym_true() -> Sym {
    SYM_TRUE.clone()
}

pub fn sym_false() -> Sym {
    SYM_FALSE.clone()
}

pub fn boolean(b: bool) -> Value {
    Value::Sym(if b { sym_true() } else { sym_false() })
}

// --- containers -------------------------------------------------------------

#[derive(Debug)]
pub struct ListData {
    pub items: Vec<Value>,
    /// Set when a handle to this node is copied. The next write clones.
    ///
    /// Atomic rather than a plain field so that marking a node shared never
    /// needs the write lock: a deep comparison reads a structure while copying
    /// every handle it passes.
    pub shared: AtomicBool,
}

/// Entries are kept in insertion order: a dict is compared key by key (§5), and
/// deterministic order keeps rendering and iteration reproducible.
#[derive(Debug)]
pub struct DictData {
    pub entries: Vec<(Sym, Value)>,
    pub shared: AtomicBool,
}

impl DictData {
    pub fn get(&self, key: &Sym) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn position(&self, key: &Sym) -> Option<usize> {
        self.entries.iter().position(|(k, _)| k == key)
    }

    pub fn set(&mut self, key: Sym, value: Value) {
        match self.position(&key) {
            Some(i) => self.entries[i].1 = value,
            None => self.entries.push((key, value)),
        }
    }
}

pub type ListRef = Arc<RwLock<ListData>>;
pub type DictRef = Arc<RwLock<DictData>>;

pub fn new_list(items: Vec<Value>) -> Value {
    Value::List(Arc::new(RwLock::new(ListData { items, shared: AtomicBool::new(false) })))
}

pub fn new_dict(entries: Vec<(Sym, Value)>) -> Value {
    Value::Dict(Arc::new(RwLock::new(DictData { entries, shared: AtomicBool::new(false) })))
}

/// Snapshot a node so the lock can be released before anything walks further.
pub fn list_items(list: &ListRef) -> Vec<Value> {
    list.read().unwrap_or_else(|e| e.into_inner()).items.clone()
}

pub fn dict_entries(dict: &DictRef) -> Vec<(Sym, Value)> {
    dict.read().unwrap_or_else(|e| e.into_inner()).entries.clone()
}

// --- closures ---------------------------------------------------------------

/// A closure captures its enclosing **scope**, not a copy of it (§5).
pub struct Closure {
    pub name: String,
    pub params: Vec<ParamInfo>,
    pub chunk: Arc<Chunk>,
    pub scope: ScopeRef,
}

impl Closure {
    pub fn required(&self) -> usize {
        self.params.iter().filter(|p| !p.has_default && !p.variadic).count()
    }

    /// How a call is described in a diagnostic: `f(&list, value)`.
    pub fn signature(&self) -> String {
        let params: Vec<String> = self
            .params
            .iter()
            .map(|p| {
                format!(
                    "{}{}{}",
                    if p.by_ref { "&" } else { "" },
                    p.name,
                    if p.variadic { "*" } else { "" }
                )
            })
            .collect();
        let name = if self.name.is_empty() { "fn" } else { &self.name };
        format!("{name}({})", params.join(", "))
    }
}

impl fmt::Debug for Closure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fn {}", self.signature())
    }
}

/// What one parameter of a builtin expects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PKind {
    /// Must be supplied.
    Plain,
    /// Must be supplied, and the call must mark it with `&` (§5.1).
    Ref,
    /// Has a default, so it may be left out.
    Default,
    /// `name*`: collects the rest of the positional arguments.
    Variadic,
    /// The bare `*`: takes nothing, and closes the positional list.
    Star,
}

/// One builtin, in one place. Everything the caller machinery needs — the
/// module it belongs to, its parameters, and how it reads in a diagnostic.
pub struct NativeInfo {
    /// `None` for the flat builtins, which are looked up after the scope chain;
    /// `Some("fs")` for one that only a `use fs` brings into reach (§7).
    pub module: Option<&'static str>,
    pub name: &'static str,
    pub params: &'static [(&'static str, PKind)],
    pub signature: &'static str,
}

/// The builtins: `alive()`, which is the language primitive of §9.5, the three
/// channel calls of `spec/hydra_channels.md`, the standard library of
/// `spec/hydra_stdlib.md`, and the `fs` module of `spec/hydra_fs.md`.
///
/// The flat ones are global names looked up *after* the scope chain, so a
/// program can shadow one — they are not reserved words. A module's are
/// reachable only through it: `fs::read`, or bare after `use fs as *`.
///
/// Two natives may share a name within a module, which is how a reader offers
/// both `read(path)` and `read(path, fallback)`: resolution by shape picks
/// between them exactly as it does for two functions a program declares (§3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Native {
    Alive,
    Print,
    Has,
    Get,
    Len,
    Push,
    Send,
    Receive,
    Channel,
    Reject,
    Return,
    Break,
    Continue,
    Exit,
    Panic,

    EnvArgs,
    EnvGet,
    EnvGetOr,
    EnvAll,
    EnvPlatform,
    TextSplit,
    TextJoin,
    TextTrim,
    TextReplace,
    TextFind,
    TextStartsWith,
    TextEndsWith,
    TextLower,
    TextUpper,
    JsonParse,
    JsonParseOr,
    TimeParse,
    TimeParseOr,
    JsonStringify,
    IoRead,
    IoReadOr,
    IoReadLine,
    IoReadLineOr,
    IoLines,
    IoLinesOr,
    IoWrite,
    IoFlush,
    IoIsTerminal,
    TimeNow,
    TimeMonotonic,
    TimeSleep,
    TimeFormat,
    HttpRequest,
    HttpRequestOr,
    HttpGet,
    HttpGetOr,
    HttpPost,
    HttpPostOr,
    HttpHead,
    HttpHeadOr,
    HttpPut,
    HttpPutOr,
    HttpPatch,
    HttpPatchOr,
    HttpDelete,
    HttpDeleteOr,
    HttpOptions,
    HttpOptionsOr,
    HttpConnect,
    HttpConnectOr,
    HttpTrace,
    HttpTraceOr,
    HttpDownload,
    HttpDownloadOr,

    ListMap,
    ListFilter,
    ListFilterMap,
    ListFlatMap,
    ListTakeWhile,
    ListSkipWhile,
    ListMapWhile,
    ListForEach,
    ListInspect,
    ListAny,
    ListAll,
    ListFind,
    ListFindMap,
    ListPosition,
    ListRposition,
    ListPartition,
    ListUniqueBy,
    ListDedupBy,
    ListDedupByKey,
    ListMinBy,
    ListMaxBy,
    ListGroupBy,
    ListChunkBy,
    ListSortedByKey,
    ListMinByKey,
    ListMaxByKey,
    ListFold,
    ListReduce,
    ListSortedBy,
    ListFlatten,
    ListEnumerate,
    ListRev,
    ListSum,
    ListProduct,
    ListMin,
    ListMax,
    ListSorted,
    ListUnique,
    ListDedup,
    ListCounts,
    ListCount,
    ListFirst,
    ListLast,
    ListTake,
    ListSkip,
    ListStepBy,
    ListChunks,
    ListWindows,
    ListIntersperse,
    ListNth,
    ListCombinations,
    ListPermutations,
    ListChain,
    ListZip,
    ListInterleave,
    ListCartesianProduct,
    ListUnzip,
    ListRange,
    ListRepeat,
    ListJoin,
    CliParser,
    CliAddArgument,
    CliAddSubparser,
    CliParseArgs,
    CliTryParseArgs,
    CliFormatHelp,
    CliFormatUsage,

    // --- fs (spec/hydra_fs.md) ---------------------------------------------
    FsJoin,
    FsParent,
    FsName,
    FsStem,
    FsExtension,
    FsAbsolute,
    FsExists,
    FsIsFile,
    FsIsDir,
    FsSize,
    FsSizeOr,
    FsModified,
    FsModifiedOr,
    FsRead,
    FsReadOr,
    FsLines,
    FsLinesOr,
    FsList,
    FsListOr,
    FsWrite,
    FsCopy,
    FsMove,
    FsRemove,
    FsMakeDir,
    FsCwd,
    FsHome,
    FsTemp,
}

use PKind::{Default as Def, Plain, Ref, Star, Variadic};

pub const NATIVES: &[Native] = &[
    Native::ListMap,
    Native::ListFilter,
    Native::ListFilterMap,
    Native::ListFlatMap,
    Native::ListTakeWhile,
    Native::ListSkipWhile,
    Native::ListMapWhile,
    Native::ListForEach,
    Native::ListInspect,
    Native::ListAny,
    Native::ListAll,
    Native::ListFind,
    Native::ListFindMap,
    Native::ListPosition,
    Native::ListRposition,
    Native::ListPartition,
    Native::ListUniqueBy,
    Native::ListDedupBy,
    Native::ListDedupByKey,
    Native::ListMinBy,
    Native::ListMaxBy,
    Native::ListGroupBy,
    Native::ListChunkBy,
    Native::ListSortedByKey,
    Native::ListMinByKey,
    Native::ListMaxByKey,
    Native::ListFold,
    Native::ListReduce,
    Native::ListSortedBy,
    Native::ListFlatten,
    Native::ListEnumerate,
    Native::ListRev,
    Native::ListSum,
    Native::ListProduct,
    Native::ListMin,
    Native::ListMax,
    Native::ListSorted,
    Native::ListUnique,
    Native::ListDedup,
    Native::ListCounts,
    Native::ListCount,
    Native::ListFirst,
    Native::ListLast,
    Native::ListTake,
    Native::ListSkip,
    Native::ListStepBy,
    Native::ListChunks,
    Native::ListWindows,
    Native::ListIntersperse,
    Native::ListNth,
    Native::ListCombinations,
    Native::ListPermutations,
    Native::ListChain,
    Native::ListZip,
    Native::ListInterleave,
    Native::ListCartesianProduct,
    Native::ListUnzip,
    Native::ListRange,
    Native::ListRepeat,
    Native::ListJoin,
    Native::CliParser,
    Native::CliAddArgument,
    Native::CliAddSubparser,
    Native::CliParseArgs,
    Native::CliTryParseArgs,
    Native::CliFormatHelp,
    Native::CliFormatUsage,

    Native::EnvArgs,
    Native::EnvGet,
    Native::EnvGetOr,
    Native::EnvAll,
    Native::EnvPlatform,
    Native::TextSplit,
    Native::TextJoin,
    Native::TextTrim,
    Native::TextReplace,
    Native::TextFind,
    Native::TextStartsWith,
    Native::TextEndsWith,
    Native::TextLower,
    Native::TextUpper,
    Native::JsonParse,
    Native::JsonParseOr,
    Native::TimeParse,
    Native::TimeParseOr,
    Native::JsonStringify,
    Native::IoRead,
    Native::IoReadOr,
    Native::IoReadLine,
    Native::IoReadLineOr,
    Native::IoLines,
    Native::IoLinesOr,
    Native::IoWrite,
    Native::IoFlush,
    Native::IoIsTerminal,
    Native::TimeNow,
    Native::TimeMonotonic,
    Native::TimeSleep,
    Native::TimeFormat,
    Native::HttpRequest,
    Native::HttpRequestOr,
    Native::HttpGet,
    Native::HttpGetOr,
    Native::HttpPost,
    Native::HttpPostOr,
    Native::HttpHead,
    Native::HttpHeadOr,
    Native::HttpPut,
    Native::HttpPutOr,
    Native::HttpPatch,
    Native::HttpPatchOr,
    Native::HttpDelete,
    Native::HttpDeleteOr,
    Native::HttpOptions,
    Native::HttpOptionsOr,
    Native::HttpConnect,
    Native::HttpConnectOr,
    Native::HttpTrace,
    Native::HttpTraceOr,
    Native::HttpDownload,
    Native::HttpDownloadOr,

    Native::Alive,
    Native::Print,
    Native::Has,
    Native::Get,
    Native::Len,
    Native::Push,
    Native::Send,
    Native::Receive,
    Native::Channel,
    Native::Reject,
    Native::Return,
    Native::Break,
    Native::Continue,
    Native::Exit,
    Native::Panic,
    Native::FsJoin,
    Native::FsParent,
    Native::FsName,
    Native::FsStem,
    Native::FsExtension,
    Native::FsAbsolute,
    Native::FsExists,
    Native::FsIsFile,
    Native::FsIsDir,
    Native::FsSize,
    Native::FsSizeOr,
    Native::FsModified,
    Native::FsModifiedOr,
    Native::FsRead,
    Native::FsReadOr,
    Native::FsLines,
    Native::FsLinesOr,
    Native::FsList,
    Native::FsListOr,
    Native::FsWrite,
    Native::FsCopy,
    Native::FsMove,
    Native::FsRemove,
    Native::FsMakeDir,
    Native::FsCwd,
    Native::FsHome,
    Native::FsTemp,
];

/// The calls that only mean something inside a trail (channels §6.4).
pub const CHANNEL_NATIVES: &[Native] = &[Native::Send, Native::Receive, Native::Channel];

/// Modules the interpreter and `check` know without a file. A file of the same
/// name shadows one, which is why that is discouraged (§7).
pub const BUILTIN_MODULES: &[&str] = &["fs", "env", "text", "json", "io", "time", "http", "list", "cli"];

impl Native {
    pub fn info(self) -> &'static NativeInfo {
        macro_rules! info {
            ($module:expr, $name:expr, $signature:expr, $params:expr) => {
                &NativeInfo {
                    module: $module,
                    name: $name,
                    params: $params,
                    signature: $signature,
                }
            };
        }
        const FS: Option<&str> = Some("fs");
        const ENV: Option<&str> = Some("env");
        const TEXT: Option<&str> = Some("text");
        const JSON: Option<&str> = Some("json");
        const IO: Option<&str> = Some("io");
        const TIME: Option<&str> = Some("time");
        const LIST: Option<&str> = Some("list");
        const CLI: Option<&str> = Some("cli");
        const HTTP: Option<&str> = Some("http");
        match self {
            Native::EnvArgs => info!(ENV, "args", "args()", &[]),
            Native::EnvGet => info!(ENV, "get", "get(name)", &[("name", Plain)]),
            Native::EnvGetOr => info!(ENV, "get", "get(name, fallback)", &[("name", Plain), ("fallback", Plain)]),
            Native::EnvAll => info!(ENV, "all", "all()", &[]),
            Native::EnvPlatform => info!(ENV, "platform", "platform()", &[]),
            Native::TextSplit => info!(TEXT, "split", "split(text, separator = :null)", &[("text", Plain), ("separator", Def)]),
            Native::TextJoin => info!(TEXT, "join", "join(parts, separator = \"\")", &[("parts", Plain), ("separator", Def)]),
            Native::TextTrim => info!(TEXT, "trim", "trim(text)", &[("text", Plain)]),
            Native::TextReplace => info!(TEXT, "replace", "replace(text, from, to)", &[("text", Plain), ("from", Plain), ("to", Plain)]),
            Native::TextFind => info!(TEXT, "find", "find(text, needle)", &[("text", Plain), ("needle", Plain)]),
            Native::TextStartsWith => info!(TEXT, "starts_with", "starts_with(text, prefix)", &[("text", Plain), ("prefix", Plain)]),
            Native::TextEndsWith => info!(TEXT, "ends_with", "ends_with(text, suffix)", &[("text", Plain), ("suffix", Plain)]),
            Native::TextLower => info!(TEXT, "lower", "lower(text)", &[("text", Plain)]),
            Native::TextUpper => info!(TEXT, "upper", "upper(text)", &[("text", Plain)]),
            Native::JsonParse => info!(JSON, "parse", "parse(text)", &[("text", Plain)]),
            Native::JsonParseOr => info!(JSON, "parse", "parse(text, fallback)", &[("text", Plain), ("fallback", Plain)]),
            Native::TimeParse => info!(TIME, "parse", "parse(text)", &[("text", Plain)]),
            Native::TimeParseOr => info!(TIME, "parse", "parse(text, fallback)", &[("text", Plain), ("fallback", Plain)]),
            Native::JsonStringify => info!(JSON, "stringify", "stringify(value, *, pretty = :false)", &[("value", Plain), ("", Star), ("pretty", Def)]),
            Native::IoRead => info!(IO, "read", "read(*, stream = :stdin)", &[("", Star), ("stream", Def)]),
            Native::IoReadOr => info!(IO, "read", "read(fallback, *, stream = :stdin)", &[("fallback", Plain), ("", Star), ("stream", Def)]),
            Native::IoReadLine => info!(IO, "read_line", "read_line(*, stream = :stdin)", &[("", Star), ("stream", Def)]),
            Native::IoReadLineOr => info!(IO, "read_line", "read_line(fallback, *, stream = :stdin)", &[("fallback", Plain), ("", Star), ("stream", Def)]),
            Native::IoLines => info!(IO, "lines", "lines(*, stream = :stdin)", &[("", Star), ("stream", Def)]),
            Native::IoLinesOr => info!(IO, "lines", "lines(fallback, *, stream = :stdin)", &[("fallback", Plain), ("", Star), ("stream", Def)]),
            Native::IoWrite => info!(IO, "write", "write(value, *, stream = :stdout)", &[("value", Plain), ("", Star), ("stream", Def)]),
            Native::IoFlush => info!(IO, "flush", "flush(stream = :stdout)", &[("stream", Def)]),
            Native::IoIsTerminal => info!(IO, "is_terminal", "is_terminal(stream = :stdout)", &[("stream", Def)]),
            Native::TimeNow => info!(TIME, "now", "now()", &[]),
            Native::TimeMonotonic => info!(TIME, "monotonic", "monotonic()", &[]),
            Native::TimeSleep => info!(TIME, "sleep", "sleep(seconds)", &[("seconds", Plain)]),
            Native::TimeFormat => info!(TIME, "format", "format(timestamp, format = \"%+\")", &[("timestamp", Plain), ("format", Def)]),
            Native::HttpRequest => info!(HTTP, "request", "request(method, url, *, body = :null, headers = {}, timeout = 30, max_bytes = 16777216)", &[("method", Plain), ("url", Plain), ("", Star), ("body", Def), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpRequestOr => info!(HTTP, "request", "request(method, url, fallback, *, body = :null, headers = {}, timeout = 30, max_bytes = 16777216)", &[("method", Plain), ("url", Plain), ("fallback", Plain), ("", Star), ("body", Def), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpGet => info!(HTTP, "get", "get(url, *, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("", Star), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpGetOr => info!(HTTP, "get", "get(url, fallback, *, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("fallback", Plain), ("", Star), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpPost => info!(HTTP, "post", "post(url, body, *, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("body", Plain), ("", Star), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpPostOr => info!(HTTP, "post", "post(url, body, fallback, *, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("body", Plain), ("fallback", Plain), ("", Star), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpHead => info!(HTTP, "head", "head(url, *, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("", Star), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpHeadOr => info!(HTTP, "head", "head(url, fallback, *, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("fallback", Plain), ("", Star), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpPut => info!(HTTP, "put", "put(url, body, *, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("body", Plain), ("", Star), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpPutOr => info!(HTTP, "put", "put(url, body, fallback, *, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("body", Plain), ("fallback", Plain), ("", Star), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpPatch => info!(HTTP, "patch", "patch(url, body, *, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("body", Plain), ("", Star), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpPatchOr => info!(HTTP, "patch", "patch(url, body, fallback, *, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("body", Plain), ("fallback", Plain), ("", Star), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpDelete => info!(HTTP, "delete", "delete(url, *, body = :null, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("", Star), ("body", Def), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpDeleteOr => info!(HTTP, "delete", "delete(url, fallback, *, body = :null, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("fallback", Plain), ("", Star), ("body", Def), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpOptions => info!(HTTP, "options", "options(url, *, body = :null, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("", Star), ("body", Def), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpOptionsOr => info!(HTTP, "options", "options(url, fallback, *, body = :null, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("fallback", Plain), ("", Star), ("body", Def), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpConnect => info!(HTTP, "connect", "connect(url, *, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("", Star), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpConnectOr => info!(HTTP, "connect", "connect(url, fallback, *, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("fallback", Plain), ("", Star), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpTrace => info!(HTTP, "trace", "trace(url, *, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("", Star), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpTraceOr => info!(HTTP, "trace", "trace(url, fallback, *, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("fallback", Plain), ("", Star), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpDownload => info!(HTTP, "download", "download(url, path, *, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("path", Plain), ("", Star), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::HttpDownloadOr => info!(HTTP, "download", "download(url, path, fallback, *, headers = {}, timeout = 30, max_bytes = 16777216)", &[("url", Plain), ("path", Plain), ("fallback", Plain), ("", Star), ("headers", Def), ("timeout", Def), ("max_bytes", Def)]),
            Native::ListMap => info!(LIST, "map", "map(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListFilter => info!(LIST, "filter", "filter(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListFilterMap => info!(LIST, "filter_map", "filter_map(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListFlatMap => info!(LIST, "flat_map", "flat_map(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListTakeWhile => info!(LIST, "take_while", "take_while(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListSkipWhile => info!(LIST, "skip_while", "skip_while(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListMapWhile => info!(LIST, "map_while", "map_while(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListForEach => info!(LIST, "for_each", "for_each(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListInspect => info!(LIST, "inspect", "inspect(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListAny => info!(LIST, "any", "any(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListAll => info!(LIST, "all", "all(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListFind => info!(LIST, "find", "find(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListFindMap => info!(LIST, "find_map", "find_map(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListPosition => info!(LIST, "position", "position(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListRposition => info!(LIST, "rposition", "rposition(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListPartition => info!(LIST, "partition", "partition(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListUniqueBy => info!(LIST, "unique_by", "unique_by(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListDedupByKey => info!(LIST, "dedup_by_key", "dedup_by_key(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListMinBy => info!(LIST, "min_by", "min_by(items, compare)", &[("items", Plain), ("compare", Plain)]),
            Native::ListMaxBy => info!(LIST, "max_by", "max_by(items, compare)", &[("items", Plain), ("compare", Plain)]),
            Native::ListDedupBy => info!(LIST, "dedup_by", "dedup_by(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListGroupBy => info!(LIST, "group_by", "group_by(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListChunkBy => info!(LIST, "chunk_by", "chunk_by(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListSortedByKey => info!(LIST, "sorted_by_key", "sorted_by_key(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListMinByKey => info!(LIST, "min_by_key", "min_by_key(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListMaxByKey => info!(LIST, "max_by_key", "max_by_key(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListFold => info!(LIST, "fold", "fold(items, initial, f)", &[("items", Plain), ("initial", Plain), ("f", Plain)]),
            Native::ListReduce => info!(LIST, "reduce", "reduce(items, f)", &[("items", Plain), ("f", Plain)]),
            Native::ListSortedBy => info!(LIST, "sorted_by", "sorted_by(items, compare)", &[("items", Plain), ("compare", Plain)]),
            Native::ListFlatten => info!(LIST, "flatten", "flatten(items)", &[("items", Plain)]),
            Native::ListEnumerate => info!(LIST, "enumerate", "enumerate(items)", &[("items", Plain)]),
            Native::ListRev => info!(LIST, "rev", "rev(items)", &[("items", Plain)]),
            Native::ListSum => info!(LIST, "sum", "sum(items)", &[("items", Plain)]),
            Native::ListProduct => info!(LIST, "product", "product(items)", &[("items", Plain)]),
            Native::ListMin => info!(LIST, "min", "min(items)", &[("items", Plain)]),
            Native::ListMax => info!(LIST, "max", "max(items)", &[("items", Plain)]),
            Native::ListSorted => info!(LIST, "sorted", "sorted(items)", &[("items", Plain)]),
            Native::ListUnique => info!(LIST, "unique", "unique(items)", &[("items", Plain)]),
            Native::ListDedup => info!(LIST, "dedup", "dedup(items)", &[("items", Plain)]),
            Native::ListCounts => info!(LIST, "counts", "counts(items)", &[("items", Plain)]),
            Native::ListCount => info!(LIST, "count", "count(items)", &[("items", Plain)]),
            Native::ListFirst => info!(LIST, "first", "first(items)", &[("items", Plain)]),
            Native::ListLast => info!(LIST, "last", "last(items)", &[("items", Plain)]),
            Native::ListTake => info!(LIST, "take", "take(items, count)", &[("items", Plain), ("count", Plain)]),
            Native::ListSkip => info!(LIST, "skip", "skip(items, count)", &[("items", Plain), ("count", Plain)]),
            Native::ListStepBy => info!(LIST, "step_by", "step_by(items, step)", &[("items", Plain), ("step", Plain)]),
            Native::ListChunks => info!(LIST, "chunks", "chunks(items, size)", &[("items", Plain), ("size", Plain)]),
            Native::ListWindows => info!(LIST, "windows", "windows(items, size)", &[("items", Plain), ("size", Plain)]),
            Native::ListIntersperse => info!(LIST, "intersperse", "intersperse(items, separator)", &[("items", Plain), ("separator", Plain)]),
            Native::ListNth => info!(LIST, "nth", "nth(items, index)", &[("items", Plain), ("index", Plain)]),
            Native::ListCombinations => info!(LIST, "combinations", "combinations(items, size)", &[("items", Plain), ("size", Plain)]),
            Native::ListPermutations => info!(LIST, "permutations", "permutations(items, size)", &[("items", Plain), ("size", Plain)]),
            Native::ListChain => info!(LIST, "chain", "chain(left, right)", &[("left", Plain), ("right", Plain)]),
            Native::ListZip => info!(LIST, "zip", "zip(left, right)", &[("left", Plain), ("right", Plain)]),
            Native::ListInterleave => info!(LIST, "interleave", "interleave(left, right)", &[("left", Plain), ("right", Plain)]),
            Native::ListCartesianProduct => info!(LIST, "cartesian_product", "cartesian_product(left, right)", &[("left", Plain), ("right", Plain)]),
            Native::ListUnzip => info!(LIST, "unzip", "unzip(items)", &[("items", Plain)]),
            Native::ListRange => info!(LIST, "range", "range(start, end = :null, step = 1)", &[("start", Plain), ("end", Def), ("step", Def)]),
            Native::ListRepeat => info!(LIST, "repeat", "repeat(value, count)", &[("value", Plain), ("count", Plain)]),
            Native::ListJoin => info!(LIST, "join", "join(items, separator = \"\")", &[("items", Plain), ("separator", Def)]),
            Native::CliParser => info!(CLI, "parser", "parser(*, prog = :null, description = \"\", epilog = \"\", add_help = :true)", &[("", Star), ("prog", Def), ("description", Def), ("epilog", Def), ("add_help", Def)]),
            Native::CliAddArgument => info!(CLI, "add_argument", "add_argument(&parser, names*, help = \"\", dest = :null, type = :string, default = :null, required = :false, action = :store, nargs = :null, choices = :null, metavar = :null, const = :null, version = \"\")", &[("parser", Ref), ("names", Variadic), ("help", Def), ("dest", Def), ("type", Def), ("default", Def), ("required", Def), ("action", Def), ("nargs", Def), ("choices", Def), ("metavar", Def), ("const", Def), ("version", Def)]),
            Native::CliAddSubparser => info!(CLI, "add_subparser", "add_subparser(&parser, name, child, *, help = \"\", dest = \"command\", required = :true)", &[("parser", Ref), ("name", Plain), ("child", Plain), ("", Star), ("help", Def), ("dest", Def), ("required", Def)]),
            Native::CliParseArgs => info!(CLI, "parse_args", "parse_args(parser, args = :null, *, strict = :true)", &[("parser", Plain), ("args", Def), ("", Star), ("strict", Def)]),
            Native::CliTryParseArgs => info!(CLI, "try_parse_args", "try_parse_args(parser, args = :null)", &[("parser", Plain), ("args", Def)]),
            Native::CliFormatHelp => info!(CLI, "format_help", "format_help(parser)", &[("parser", Plain)]),
            Native::CliFormatUsage => info!(CLI, "format_usage", "format_usage(parser)", &[("parser", Plain)]),
            Native::Alive => info!(None, "alive", "alive()", &[]),
            // Not `end`: that keyword closes every block, so it can never be a
            // name. `terminator` follows Swift's print (hydra_stdlib.md §2).
            Native::Print => info!(
                None,
                "print",
                "print(value, terminator = \"\\n\")",
                &[("value", Plain), ("terminator", Def)]
            ),
            Native::Has => {
                info!(None, "has", "has(container, key)", &[("container", Plain), ("key", Plain)])
            }
            Native::Get => info!(
                None,
                "get",
                "get(container, key, fallback)",
                &[("container", Plain), ("key", Plain), ("fallback", Plain)]
            ),
            Native::Len => info!(None, "len", "len(value)", &[("value", Plain)]),
            Native::Push => {
                info!(None, "push", "push(&list, value)", &[("list", Ref), ("value", Plain)])
            }
            // `to` and `from` are the `*`, so `mode` can only be named.
            Native::Send => info!(
                None,
                "send",
                "send(value, to*, mode = :wait)",
                &[("value", Plain), ("to", Variadic), ("mode", Def)]
            ),
            Native::Receive => info!(None, "receive", "receive(from*)", &[("from", Variadic)]),
            Native::Channel => info!(None, "channel", "channel()", &[]),
            // `reject(msg)` constructs one tagged list: [:reject, msg].
            Native::Return => info!(None, "return", "return(values*)", &[("values", Variadic)]),
            Native::Break => info!(None, "break", "break()", &[]),
            Native::Continue => info!(None, "continue", "continue()", &[]),
            Native::Exit => info!(None, "exit", "exit(code = 0)", &[("code", Def)]),
            Native::Panic => info!(None, "panic", "panic(msg)", &[("msg", Plain)]),
            Native::Reject => {
                info!(None, "reject", "reject(msg = :null)", &[("msg", Def)])
            }

            // --- fs: paths ------------------------------------------------
            Native::FsJoin => info!(
                FS,
                "join",
                "join(base, parts*)",
                &[("base", Plain), ("parts", Variadic)]
            ),
            Native::FsParent => info!(FS, "parent", "parent(path)", &[("path", Plain)]),
            Native::FsName => info!(FS, "name", "name(path)", &[("path", Plain)]),
            Native::FsStem => info!(FS, "stem", "stem(path)", &[("path", Plain)]),
            Native::FsExtension => {
                info!(FS, "extension", "extension(path)", &[("path", Plain)])
            }
            Native::FsAbsolute => info!(FS, "absolute", "absolute(path)", &[("path", Plain)]),

            // --- fs: asking -----------------------------------------------
            Native::FsExists => info!(FS, "exists", "exists(path)", &[("path", Plain)]),
            Native::FsIsFile => info!(FS, "is_file", "is_file(path)", &[("path", Plain)]),
            Native::FsIsDir => info!(FS, "is_dir", "is_dir(path)", &[("path", Plain)]),
            Native::FsSize => info!(FS, "size", "size(path)", &[("path", Plain)]),
            Native::FsSizeOr => info!(
                FS,
                "size",
                "size(path, fallback)",
                &[("path", Plain), ("fallback", Plain)]
            ),
            Native::FsModified => info!(FS, "modified", "modified(path)", &[("path", Plain)]),
            Native::FsModifiedOr => info!(
                FS,
                "modified",
                "modified(path, fallback)",
                &[("path", Plain), ("fallback", Plain)]
            ),

            // --- fs: reading ----------------------------------------------
            Native::FsRead => info!(FS, "read", "read(path)", &[("path", Plain)]),
            Native::FsReadOr => info!(
                FS,
                "read",
                "read(path, fallback)",
                &[("path", Plain), ("fallback", Plain)]
            ),
            Native::FsLines => info!(FS, "lines", "lines(path)", &[("path", Plain)]),
            Native::FsLinesOr => info!(
                FS,
                "lines",
                "lines(path, fallback)",
                &[("path", Plain), ("fallback", Plain)]
            ),
            // Every flag is keyword-only, which is also what keeps the two
            // `list`s apart: a second positional can only be the fallback.
            Native::FsList => info!(
                FS,
                "list",
                "list(dir, *, match = \"*\", recursive = :false)",
                &[("dir", Plain), ("", Star), ("match", Def), ("recursive", Def)]
            ),
            Native::FsListOr => info!(
                FS,
                "list",
                "list(dir, fallback, *, match = \"*\", recursive = :false)",
                &[
                    ("dir", Plain),
                    ("fallback", Plain),
                    ("", Star),
                    ("match", Def),
                    ("recursive", Def)
                ]
            ),

            // --- fs: writing ----------------------------------------------
            Native::FsWrite => info!(
                FS,
                "write",
                "write(path, text, *, mode = :replace, parents = :true)",
                &[
                    ("path", Plain),
                    ("text", Plain),
                    ("", Star),
                    ("mode", Def),
                    ("parents", Def)
                ]
            ),
            Native::FsCopy => info!(
                FS,
                "copy",
                "copy(source, target, *, overwrite = :true, parents = :true)",
                &[
                    ("source", Plain),
                    ("target", Plain),
                    ("", Star),
                    ("overwrite", Def),
                    ("parents", Def)
                ]
            ),
            Native::FsMove => info!(
                FS,
                "move",
                "move(source, target, *, overwrite = :false, parents = :true)",
                &[
                    ("source", Plain),
                    ("target", Plain),
                    ("", Star),
                    ("overwrite", Def),
                    ("parents", Def)
                ]
            ),
            Native::FsRemove => info!(
                FS,
                "remove",
                "remove(path, *, recursive = :false)",
                &[("path", Plain), ("", Star), ("recursive", Def)]
            ),
            Native::FsMakeDir => info!(
                FS,
                "make_dir",
                "make_dir(path, *, parents = :true)",
                &[("path", Plain), ("", Star), ("parents", Def)]
            ),

            // --- fs: where the process is ---------------------------------
            Native::FsCwd => info!(FS, "cwd", "cwd()", &[]),
            Native::FsHome => info!(FS, "home", "home()", &[]),
            Native::FsTemp => info!(FS, "temp", "temp()", &[]),
        }
    }

    /// The flat builtins, which unqualified lookup falls back to (§7). A
    /// module's are not among them.
    pub fn lookup(name: &str) -> Option<Native> {
        NATIVES.iter().copied().find(|n| {
            let info = n.info();
            info.module.is_none() && info.name == name
        })
    }

    /// Every builtin a module has under that name — more than one where it
    /// offers overloads, and in the order they are tried.
    pub fn in_module(module: &str, name: &str) -> Vec<Native> {
        NATIVES
            .iter()
            .copied()
            .filter(|n| {
                let info = n.info();
                info.module == Some(module) && info.name == name
            })
            .collect()
    }

    /// Every name a builtin module exports, for `use fs as *` and for `check`.
    pub fn module_names(module: &str) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = NATIVES
            .iter()
            .filter(|n| n.info().module == Some(module))
            .map(|n| n.info().name)
            .collect();
        names.dedup();
        names
    }

    pub fn name(self) -> &'static str {
        self.info().name
    }

    pub fn module(self) -> Option<&'static str> {
        self.info().module
    }

    /// Arguments that must be supplied.
    pub fn required(self) -> usize {
        self.info()
            .params
            .iter()
            .filter(|(_, kind)| matches!(kind, PKind::Plain | PKind::Ref))
            .count()
    }

    /// Arguments it accepts at most; the difference from [`Native::required`]
    /// is the defaults.
    pub fn total(self) -> usize {
        self.info().params.len()
    }

    /// Which parameter is the `*`, if any: it collects what is left of the
    /// positional arguments, and everything after it is keyword-only
    /// (channels §6.1).
    pub fn variadic(self) -> Option<usize> {
        self.info()
            .params
            .iter()
            .position(|(_, kind)| matches!(kind, PKind::Variadic | PKind::Star))
    }

    /// Parameter names, so a call can name its arguments (§3).
    pub fn param_names(self) -> Vec<&'static str> {
        self.info().params.iter().map(|(name, _)| *name).collect()
    }

    /// Which parameters the call must mark with `&` (§5.1).
    pub fn by_ref(self) -> Vec<bool> {
        self.info().params.iter().map(|(_, kind)| *kind == PKind::Ref).collect()
    }

    pub fn has_default(self, index: usize) -> bool {
        matches!(self.info().params.get(index), Some((_, PKind::Default)))
    }

    pub fn signature(self) -> &'static str {
        self.info().signature
    }

    /// How many values a call to it answers with. A reader that took a
    /// fallback says why it fell back (fs §1), and `receive` says which trail
    /// sent the value (channels §1).
    pub fn returns(self) -> usize {
        match self {
            Native::CliTryParseArgs => 3,
            Native::ListPartition |
            Native::ListUnzip |
            Native::CliParseArgs |
            Native::EnvGet |
            Native::EnvGetOr |
            Native::JsonParse |
            Native::JsonParseOr |
            Native::TimeParse |
            Native::TimeParseOr |
            Native::IoRead |
            Native::IoReadOr |
            Native::IoReadLine |
            Native::IoReadLineOr |
            Native::IoLines |
            Native::IoLinesOr |
            Native::HttpRequest |
            Native::HttpRequestOr |
            Native::HttpGet |
            Native::HttpGetOr |
            Native::HttpPost |
            Native::HttpPostOr |
            Native::HttpHead |
            Native::HttpHeadOr |
            Native::HttpPut |
            Native::HttpPutOr |
            Native::HttpPatch |
            Native::HttpPatchOr |
            Native::HttpDelete |
            Native::HttpDeleteOr |
            Native::HttpOptions |
            Native::HttpOptionsOr |
            Native::HttpConnect |
            Native::HttpConnectOr |
            Native::HttpTrace |
            Native::HttpTraceOr |
            Native::HttpDownload |
            Native::HttpDownloadOr |
            Native::Receive
            | Native::FsSizeOr
            | Native::FsModifiedOr
            | Native::FsReadOr
            | Native::FsLinesOr
            | Native::FsListOr
            | Native::FsWrite => 2,
            _ => 1,
        }
    }
}

// --- references -------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum PathSeg {
    Key(Sym),
    /// Negative counts from the end and is resolved against the list's length
    /// by [`resolve_index`].
    Index(isize),
}

/// `&lvalue`: the root variable's cell plus the path from it (§5.1).
#[derive(Clone)]
pub struct RefValue {
    pub root: Cell,
    pub path: Arc<Vec<PathSeg>>,
}

impl fmt::Debug for RefValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "&{:?}", self.path)
    }
}

// --- values -----------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Value {
    Num(f64),
    Str(Arc<str>),
    Sym(Sym),
    List(ListRef),
    Dict(DictRef),
    Fn(Arc<Closure>),
    Native(Native),
    /// Only ever lives in a binding, a dict entry or a list element: reads
    /// dereference it transparently.
    Ref(RefValue),
}

impl Value {
    pub fn null() -> Value {
        Value::Sym(sym_null())
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Value::Num(_) => "number",
            Value::Str(_) => "string",
            Value::Sym(_) => "symbol",
            Value::List(_) => "list",
            Value::Dict(_) => "dict",
            Value::Fn(_) | Value::Native(_) => "closure",
            Value::Ref(_) => "reference",
        }
    }

    /// `:null` and `:false` are falsy. *Everything else is truthy*, including
    /// `0`, `""` and `[]` (§5).
    pub fn truthy(&self) -> bool {
        match self {
            Value::Sym(s) => *s != sym_null() && *s != sym_false(),
            _ => true,
        }
    }

    /// The number this is, or a crash naming the operator that wanted one.
    ///
    /// `what` is borrowed and the message is built only on the failing path:
    /// this runs on every arithmetic operation, and formatting one string per
    /// operation costs more than the operation.
    pub fn as_num(&self, what: &str) -> Result<f64, Crash> {
        match self {
            Value::Num(n) => Ok(*n),
            other => Err(Crash::new(format!("`{what}` needs a number, got a {}", other.kind()))),
        }
    }
}

// --- copying (§5.1) ---------------------------------------------------------

/// The copy every binding, argument, insertion and return performs.
///
/// Containers are not walked: the node is marked shared and the handle cloned,
/// so the deep copy is paid for only if someone writes.
pub fn copy_value(v: &Value) -> Value {
    match v {
        Value::List(rc) => {
            rc.read().unwrap_or_else(|e| e.into_inner()).shared.store(true, Ordering::Relaxed);
            Value::List(rc.clone())
        }
        Value::Dict(rc) => {
            rc.read().unwrap_or_else(|e| e.into_inner()).shared.store(true, Ordering::Relaxed);
            Value::Dict(rc.clone())
        }
        other => other.clone(),
    }
}

/// Give `slot` a node of its own if the one it holds is shared, marking the
/// children shared in turn because they now have two parents.
fn unshare(slot: &mut Value) {
    match slot {
        Value::List(rc) => {
            let items = {
                let data = rc.read().unwrap_or_else(|e| e.into_inner());
                if !data.shared.load(Ordering::Relaxed) {
                    return;
                }
                data.items.iter().map(copy_value).collect()
            };
            *rc = Arc::new(RwLock::new(ListData { items, shared: AtomicBool::new(false) }));
        }
        Value::Dict(rc) => {
            let entries = {
                let data = rc.read().unwrap_or_else(|e| e.into_inner());
                if !data.shared.load(Ordering::Relaxed) {
                    return;
                }
                data.entries.iter().map(|(k, v)| (k.clone(), copy_value(v))).collect()
            };
            *rc = Arc::new(RwLock::new(DictData { entries, shared: AtomicBool::new(false) }));
        }
        _ => {}
    }
}

// --- indexing ---------------------------------------------------------------

/// Resolve a list index against a length: 0-based, and a negative index counts
/// from the end, so `a[-1]` is the last element. Anything still outside the
/// list crashes, the way a missing key does (§5, §15.2).
pub fn resolve_index(raw: isize, len: usize) -> Result<usize, Crash> {
    let resolved = if raw < 0 { raw + len as isize } else { raw };
    if resolved < 0 || resolved as usize >= len {
        return Err(Crash::new(format!(
            "list index {raw} is out of range for a list of {len} element(s)"
        )));
    }
    Ok(resolved as usize)
}

/// Turn an evaluated key into a path segment.
///
/// `d.k` is exactly `d[:k]` (§5), so there is one code path here and not two.
pub fn path_segment(key: &Value) -> Result<PathSeg, Crash> {
    match key {
        Value::Sym(s) => Ok(PathSeg::Key(s.clone())),
        Value::Num(n) => {
            if !n.is_finite() || n.fract() != 0.0 {
                return Err(Crash::new(format!(
                    "list index must be a whole number, got {}",
                    num_to_text(*n)
                )));
            }
            Ok(PathSeg::Index(*n as isize))
        }
        other => Err(Crash::new(format!(
            "a key must be a symbol and a list index a number, got a {}",
            other.kind()
        ))),
    }
}

fn missing(seg: &PathSeg) -> Crash {
    match seg {
        PathSeg::Key(s) => Crash::new(format!("no key .{} in this dict", s.name())),
        PathSeg::Index(i) => Crash::new(format!("list index {i} is out of range")),
    }
}

// --- reading and writing places --------------------------------------------

/// Where a walk has to start over because it met a `&`.
///
/// Following a reference from inside a structure means jumping to another root,
/// so the walk returns this instead: every lock it holds is released, and it
/// starts again from there. That is what keeps locks strictly root-to-leaf.
enum Walk<T> {
    Done(T),
    Redirect { root: Cell, path: Vec<PathSeg> },
}

fn redirect<T>(reference: &RefValue, rest: &[PathSeg]) -> Walk<T> {
    let mut path = reference.path.as_ref().clone();
    path.extend_from_slice(rest);
    Walk::Redirect { root: reference.root.clone(), path }
}

/// One step of a read, or `None` when the key or index is simply absent. The
/// lock is released before the value is looked at any further.
fn member_of(container: &Value, seg: &PathSeg) -> Result<Option<Value>, Crash> {
    match (container, seg) {
        (Value::Dict(rc), PathSeg::Key(s)) => {
            Ok(rc.read().unwrap_or_else(|e| e.into_inner()).get(s).cloned())
        }
        (Value::List(rc), PathSeg::Index(i)) => {
            let data = rc.read().unwrap_or_else(|e| e.into_inner());
            match resolve_index(*i, data.items.len()) {
                Ok(at) => Ok(Some(data.items[at].clone())),
                Err(_) => Ok(None),
            }
        }
        (Value::List(_), PathSeg::Key(s)) => {
            Err(Crash::new(format!("a list has no key .{}; index it with a number", s.name())))
        }
        (Value::Dict(_), PathSeg::Index(i)) => {
            Err(Crash::new(format!("a dict has no index {i}; its keys are symbols")))
        }
        (other, _) => Err(Crash::new(format!("cannot index a {}", other.kind()))),
    }
}

/// Read one step: `container[key]`. Reading a missing key is a crash (§5).
pub fn get_member(container: &Value, key: &Value) -> Result<Value, Crash> {
    let container = deref(container)?;
    let seg = path_segment(key)?;
    match member_of(&container, &seg)? {
        Some(value) => read_through(&value),
        None => Err(missing(&seg)),
    }
}

/// `container[key]`, or `None` when the key or index is simply absent.
///
/// Reading a missing key crashes (§5); `has` and `get` are how a program asks
/// without crashing, so they need a lookup that can answer "no".
pub fn member_opt(container: &Value, key: &Value) -> Result<Option<Value>, Crash> {
    let container = deref(container)?;
    let seg = path_segment(key)?;
    match member_of(&container, &seg)? {
        Some(value) => read_through(&value).map(Some),
        None => Ok(None),
    }
}

/// A value read out of a container: dereference it, then copy it (§5.1).
fn read_through(v: &Value) -> Result<Value, Crash> {
    match v {
        Value::Ref(r) => read_place(&r.root, &r.path),
        other => Ok(copy_value(other)),
    }
}

/// Resolve a reference to the value it names.
pub fn deref(v: &Value) -> Result<Value, Crash> {
    match v {
        Value::Ref(r) => read_place(&r.root, &r.path),
        other => Ok(other.clone()),
    }
}

pub fn read_place(root: &Cell, path: &[PathSeg]) -> Result<Value, Crash> {
    let mut root = root.clone();
    let mut path = path.to_vec();
    for _ in 0..MAX_REDIRECTS {
        // The root's own lock is released before the walk begins.
        let mut current = root.read().unwrap_or_else(|e| e.into_inner()).clone();
        let mut rest = path.as_slice();
        let mut jump = None;
        loop {
            if let Value::Ref(r) = &current {
                jump = Some(redirect::<()>(r, rest));
                break;
            }
            let Some((seg, tail)) = rest.split_first() else { break };
            match member_of(&current, seg)? {
                Some(value) => current = value,
                None => return Err(missing(seg)),
            }
            rest = tail;
        }
        match jump {
            Some(Walk::Redirect { root: next, path: rest }) => {
                root = next;
                path = rest;
            }
            _ => return Ok(copy_value(&current)),
        }
    }
    Err(Crash::new("a cycle of references was followed while reading"))
}

/// Write `value` at `root` + `path`, path-copying shared nodes on the way (§5.1).
///
/// Writing a missing key **creates** it; reading one crashes (§5).
pub fn write_place(root: &Cell, path: &[PathSeg], value: Value) -> Result<(), Crash> {
    let mut root = root.clone();
    let mut path = path.to_vec();
    for _ in 0..MAX_REDIRECTS {
        let step = {
            let mut guard = root.write().unwrap_or_else(|e| e.into_inner());
            write_slot(&mut guard, &path, value.clone())?
        };
        match step {
            Walk::Done(()) => return Ok(()),
            Walk::Redirect { root: next, path: rest } => {
                root = next;
                path = rest;
            }
        }
    }
    Err(Crash::new("a cycle of references was followed while assigning"))
}

fn write_slot(slot: &mut Value, path: &[PathSeg], value: Value) -> Result<Walk<()>, Crash> {
    // A reference in the slot is written *through*: `f(&a)` with `x = 5` in the
    // body sets the caller's `a` (see QUESTIONS.md §6).
    if let Value::Ref(r) = slot {
        return Ok(redirect(r, path));
    }

    let Some(seg) = path.first() else {
        *slot = value;
        return Ok(Walk::Done(()));
    };

    unshare(slot);
    let last = path.len() == 1;
    match slot {
        Value::Dict(rc) => {
            let PathSeg::Key(key) = seg else {
                return Err(Crash::new("a dict is keyed by symbols, not by index"));
            };
            let mut data = rc.write().unwrap_or_else(|e| e.into_inner());
            match data.position(key) {
                Some(i) => write_slot(&mut data.entries[i].1, &path[1..], value),
                None if last => {
                    // Key writes create (§5).
                    data.entries.push((key.clone(), value));
                    Ok(Walk::Done(()))
                }
                None => Err(Crash::new(format!("no key .{} in this dict", key.name()))),
            }
        }
        Value::List(rc) => {
            let PathSeg::Index(i) = seg else {
                let PathSeg::Key(k) = seg else { unreachable!() };
                return Err(Crash::new(format!(
                    "a list has no key .{}; index it with a number",
                    k.name()
                )));
            };
            let mut data = rc.write().unwrap_or_else(|e| e.into_inner());
            let len = data.items.len();
            let at = resolve_index(*i, len)?;
            write_slot(&mut data.items[at], &path[1..], value)
        }
        other => Err(Crash::new(format!("cannot assign into a {}", other.kind()))),
    }
}

/// Apply `op` to the value at `root` + `path` and `operand`, and write the
/// result back. This is what `place += v` does.
///
/// The read and the write are **one step**: the root binding's lock is taken
/// once and held across both, so no other trail can write to that place in
/// between. `a += 1` in two trails therefore adds two, where a `Load` followed
/// by a `Store` would let one increment overwrite the other — the update is
/// atomic with respect to the place it names.
///
/// This does not make the *statement* atomic, and nothing here promises more
/// than the place: the operand was evaluated before the lock was taken, so
/// `a += b` reads `b` as it was, and `a` and `b` are still last-write-wins with
/// respect to each other (§9.2).
pub fn update_place(
    root: &Cell,
    path: &[PathSeg],
    op: &str,
    operand: &Value,
) -> Result<(), Crash> {
    let mut root = root.clone();
    let mut path = path.to_vec();
    for _ in 0..MAX_REDIRECTS {
        let step = {
            let mut guard = root.write().unwrap_or_else(|e| e.into_inner());
            update_slot(&mut guard, &path, op, operand)?
        };
        match step {
            Walk::Done(()) => return Ok(()),
            // A `&` in the way sends the update to another root. The lock here
            // goes first, exactly as a write does, and the read-modify-write
            // then happens whole under the lock of the root that really holds
            // the value.
            Walk::Redirect { root: next, path: rest } => {
                root = next;
                path = rest;
            }
        }
    }
    Err(Crash::new("a cycle of references was followed while updating"))
}

fn update_slot(
    slot: &mut Value,
    path: &[PathSeg],
    op: &str,
    operand: &Value,
) -> Result<Walk<()>, Crash> {
    if let Value::Ref(r) = slot {
        return Ok(redirect(r, path));
    }

    let Some(seg) = path.first() else {
        // The read and the write, with the lock on this slot's owner held
        // across both. `binary_op` runs on the value that is there now, and
        // nothing can look at the slot until the result is in it.
        *slot = binary_op(op, &*slot, operand)?;
        return Ok(Walk::Done(()));
    };

    unshare(slot);
    match slot {
        Value::Dict(rc) => {
            let PathSeg::Key(key) = seg else {
                return Err(Crash::new("a dict is keyed by symbols, not by index"));
            };
            let mut data = rc.write().unwrap_or_else(|e| e.into_inner());
            match data.position(key) {
                Some(i) => update_slot(&mut data.entries[i].1, &path[1..], op, operand),
                // A plain `=` would create the key (§5); this one reads it
                // first, and reading a missing key crashes — the same crash
                // `d.k = d.k + 1` would raise on its way to the write.
                None => Err(Crash::new(format!("no key .{} in this dict", key.name()))),
            }
        }
        Value::List(rc) => {
            let PathSeg::Index(i) = seg else {
                let PathSeg::Key(k) = seg else { unreachable!() };
                return Err(Crash::new(format!(
                    "a list has no key .{}; index it with a number",
                    k.name()
                )));
            };
            let mut data = rc.write().unwrap_or_else(|e| e.into_inner());
            let len = data.items.len();
            let at = resolve_index(*i, len)?;
            update_slot(&mut data.items[at], &path[1..], op, operand)
        }
        other => Err(Crash::new(format!("cannot assign into a {}", other.kind()))),
    }
}

/// Append to the list at `root` + `path`, path-copying shared nodes on the way,
/// and answer the new length. This is what `push(&list, v)` does.
pub fn push_place(root: &Cell, path: &[PathSeg], value: Value) -> Result<usize, Crash> {
    let mut root = root.clone();
    let mut path = path.to_vec();
    for _ in 0..MAX_REDIRECTS {
        let step = {
            let mut guard = root.write().unwrap_or_else(|e| e.into_inner());
            push_slot(&mut guard, &path, value.clone())?
        };
        match step {
            Walk::Done(len) => return Ok(len),
            Walk::Redirect { root: next, path: rest } => {
                root = next;
                path = rest;
            }
        }
    }
    Err(Crash::new("a cycle of references was followed while appending"))
}

fn push_slot(slot: &mut Value, path: &[PathSeg], value: Value) -> Result<Walk<usize>, Crash> {
    if let Value::Ref(r) = slot {
        return Ok(redirect(r, path));
    }

    unshare(slot);
    let Some(seg) = path.first() else {
        return match slot {
            Value::List(rc) => {
                let mut data = rc.write().unwrap_or_else(|e| e.into_inner());
                data.items.push(value);
                Ok(Walk::Done(data.items.len()))
            }
            other => Err(Crash::new(format!("`push` appends to a list, got a {}", other.kind()))),
        };
    };

    match slot {
        Value::Dict(rc) => {
            let PathSeg::Key(key) = seg else {
                return Err(Crash::new("a dict is keyed by symbols, not by index"));
            };
            let mut data = rc.write().unwrap_or_else(|e| e.into_inner());
            match data.position(key) {
                Some(i) => push_slot(&mut data.entries[i].1, &path[1..], value),
                None => Err(Crash::new(format!("no key .{} in this dict", key.name()))),
            }
        }
        Value::List(rc) => {
            let PathSeg::Index(i) = seg else {
                return Err(Crash::new("a list is indexed by number"));
            };
            let mut data = rc.write().unwrap_or_else(|e| e.into_inner());
            let len = data.items.len();
            let at = resolve_index(*i, len)?;
            push_slot(&mut data.items[at], &path[1..], value)
        }
        other => Err(Crash::new(format!("cannot append inside a {}", other.kind()))),
    }
}

// --- equality (§5) ----------------------------------------------------------

/// `===`: by identity. For values with no identity of their own — numbers,
/// strings, symbols — that is value equality, so `"a" === "a"` is true. `NaN`
/// is never equal to itself under either operator.
pub fn identical(a: &Value, b: &Value) -> bool {
    // A reference is transparent: `c := &a` gives `c === a` (§5.1).
    if matches!(a, Value::Ref(_)) || matches!(b, Value::Ref(_)) {
        let (Ok(a), Ok(b)) = (deref(a), deref(b)) else { return false };
        return identical(&a, &b);
    }
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Sym(x), Value::Sym(y)) => x == y,
        (Value::List(x), Value::List(y)) => Arc::ptr_eq(x, y),
        (Value::Dict(x), Value::Dict(y)) => Arc::ptr_eq(x, y),
        (Value::Fn(x), Value::Fn(y)) => Arc::ptr_eq(x, y),
        (Value::Native(x), Value::Native(y)) => x == y,
        _ => false,
    }
}

/// `==`: deep comparison, cycle-safe.
///
/// Identity is tried first and settles it when it holds (§5), which is the fast
/// path for comparing a value with itself — and the reason a structure holding
/// `NaN` is `==` to itself.
pub fn deep_equal(a: &Value, b: &Value) -> bool {
    let mut visited: Vec<(usize, usize)> = Vec::new();
    deep_equal_inner(a, b, &mut visited, 0)
}

fn node_pair(x: &impl AsPtr, y: &impl AsPtr) -> (usize, usize) {
    (x.as_addr(), y.as_addr())
}

/// So the visited-pair set can hold either kind of node.
trait AsPtr {
    fn as_addr(&self) -> usize;
}

impl AsPtr for ListRef {
    fn as_addr(&self) -> usize {
        Arc::as_ptr(self) as *const u8 as usize
    }
}

impl AsPtr for DictRef {
    fn as_addr(&self) -> usize {
        Arc::as_ptr(self) as *const u8 as usize
    }
}

fn deep_equal_inner(a: &Value, b: &Value, visited: &mut Vec<(usize, usize)>, depth: u32) -> bool {
    if identical(a, b) {
        return true;
    }
    if matches!(a, Value::Ref(_)) || matches!(b, Value::Ref(_)) {
        let (Ok(a), Ok(b)) = (deref(a), deref(b)) else { return false };
        return deep_equal_inner(&a, &b, visited, depth);
    }
    // The visited-pair set is only allocated once recursion passes a small
    // depth (§5).
    let tracking = depth > 8;
    match (a, b) {
        (Value::List(x), Value::List(y)) => {
            if tracking {
                let pair = node_pair(x, y);
                if visited.contains(&pair) {
                    return true;
                }
                visited.push(pair);
            }
            // Snapshot both nodes and let the locks go before recursing: a `&`
            // inside could lead the walk back to either of them.
            let (x, y) = (list_items(x), list_items(y));
            let equal = x.len() == y.len()
                && x.iter().zip(y.iter()).all(|(p, q)| deep_equal_inner(p, q, visited, depth + 1));
            if tracking {
                visited.pop();
            }
            equal
        }
        (Value::Dict(x), Value::Dict(y)) => {
            if tracking {
                let pair = node_pair(x, y);
                if visited.contains(&pair) {
                    return true;
                }
                visited.push(pair);
            }
            let (x, y) = (dict_entries(x), dict_entries(y));
            let equal = x.len() == y.len()
                && x.iter().all(|(k, v)| match y.iter().find(|(key, _)| key == k) {
                    Some((_, other)) => deep_equal_inner(v, other, visited, depth + 1),
                    None => false,
                });
            if tracking {
                visited.pop();
            }
            equal
        }
        // Closures have no structural equality; `==` falls back to identity,
        // which was already tried above (§5).
        _ => false,
    }
}

// --- text -------------------------------------------------------------------

/// JavaScript-shaped number rendering, which is what the spec follows for
/// numbers everywhere else (§2).
pub fn num_to_text(n: f64) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if n == 0.0 {
        return "0".to_string();
    }
    format!("{n}")
}

/// The text form of a value: what `\(value)` renders and what `print` writes.
pub fn to_text(v: &Value) -> String {
    to_text_at(v, 0)
}

/// Rendering follows references, and a structure can hold a reference to
/// itself, so the walk is depth-limited.
const TEXT_DEPTH_LIMIT: u32 = 16;

fn to_text_at(v: &Value, depth: u32) -> String {
    if depth > TEXT_DEPTH_LIMIT {
        return "…".to_string();
    }
    let to_text = |v: &Value| to_text_at(v, depth + 1);
    match v {
        Value::Num(n) => num_to_text(*n),
        Value::Str(s) => s.to_string(),
        Value::Sym(s) => {
            if crate::lexer::is_symbol_name(s.name()) {
                format!(":{}", s.name())
            } else {
                format!(":\"{}\"", crate::lexer::escape_string(s.name()))
            }
        }
        Value::List(rc) => {
            let items: Vec<String> = list_items(rc).iter().map(to_text).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Dict(rc) => {
            let entries = dict_entries(rc);
            if entries.is_empty() {
                return "{}".to_string();
            }
            let entries: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{} : {}", to_text(&Value::Sym(k.clone())), to_text(v)))
                .collect();
            format!("{{ {} }}", entries.join(", "))
        }
        Value::Fn(c) => format!("fn {}", c.signature()),
        Value::Native(n) => format!("fn {}()", n.name()),
        Value::Ref(r) => match read_place(&r.root, &r.path) {
            Ok(v) => to_text(&v),
            Err(_) => "&<unreadable>".to_string(),
        },
    }
}

// --- operators (§2, §5) -----------------------------------------------------

/// ToInt32, JavaScript-style: `NaN` and the infinities convert to 0, everything
/// else truncates toward zero and wraps modulo 2^32 (§2).
pub fn to_int32(x: f64) -> i32 {
    to_uint32(x) as i32
}

pub fn to_uint32(x: f64) -> u32 {
    if !x.is_finite() {
        return 0;
    }
    let truncated = x.trunc();
    let wrapped = truncated.rem_euclid(4294967296.0);
    wrapped as u32
}

fn int_result(n: i32) -> Value {
    Value::Num(n as f64)
}

pub fn binary_op(op: &str, a: &Value, b: &Value) -> Result<Value, Crash> {
    match op {
        "==" => return Ok(boolean(deep_equal(a, b))),
        "!=" => return Ok(boolean(!deep_equal(a, b))),
        "===" => return Ok(boolean(identical(a, b))),
        "!==" => return Ok(boolean(!identical(a, b))),
        _ => {}
    }

    // `+` concatenates two strings. It does *not* convert: interpolation is how
    // a value is rendered, so `"n = " + 3` is a bad operand rather than a
    // silent conversion — write `"n = \(3)"`.
    if op == "+" {
        match (a, b) {
            (Value::Str(x), Value::Str(y)) => {
                return Ok(Value::Str(Arc::from(format!("{x}{y}").as_str())))
            }
            (Value::Str(_), other) | (other, Value::Str(_)) => {
                return Err(Crash::new(format!(
                    "`+` joins two strings or adds two numbers, got a string and a {}; \
                     interpolate instead, as in \"…\\(x)…\"",
                    other.kind()
                )))
            }
            _ => {}
        }
    }

    match op {
        "+" | "-" | "*" | "/" | "%" | "<" | ">" | "<=" | ">=" => {
            let x = a.as_num(op)?;
            let y = b.as_num(op)?;
            Ok(match op {
                "+" => Value::Num(x + y),
                "-" => Value::Num(x - y),
                "*" => Value::Num(x * y),
                "/" => Value::Num(x / y),
                "%" => Value::Num(x % y),
                "<" => boolean(x < y),
                ">" => boolean(x > y),
                "<=" => boolean(x <= y),
                ">=" => boolean(x >= y),
                _ => unreachable!(),
            })
        }
        "|" | "&" | "^" | "<<" | ">>" | ">>>" => {
            let x = a.as_num(op)?;
            let y = b.as_num(op)?;
            Ok(match op {
                "|" => int_result(to_int32(x) | to_int32(y)),
                "&" => int_result(to_int32(x) & to_int32(y)),
                "^" => int_result(to_int32(x) ^ to_int32(y)),
                // Shift counts are taken modulo 32 (§2).
                "<<" => int_result(to_int32(x).wrapping_shl(to_uint32(y) & 31)),
                ">>" => int_result(to_int32(x).wrapping_shr(to_uint32(y) & 31)),
                // The one operator that can produce a value above 2^31 - 1 (§2).
                ">>>" => Value::Num((to_uint32(x) >> (to_uint32(y) & 31)) as f64),
                _ => unreachable!(),
            })
        }
        other => Err(Crash::new(format!("unknown operator `{other}`"))),
    }
}

pub fn unary_op(op: &str, v: &Value) -> Result<Value, Crash> {
    match op {
        "-" => Ok(Value::Num(-v.as_num("-")?)),
        "~" => Ok(int_result(!to_int32(v.as_num("~")?))),
        "not" => Ok(boolean(!v.truthy())),
        other => Err(Crash::new(format!("unknown operator `{other}`"))),
    }
}
