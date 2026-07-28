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

use std::cell::{Cell as StdCell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::rc::{Rc, Weak};

use crate::compile::Chunk;
use crate::errors::Crash;
use crate::scope::ScopeRef;

/// The storage a name refers to. `&` points at one of these plus a path.
pub type Cell = Rc<RefCell<Value>>;

pub fn cell(value: Value) -> Cell {
    Rc::new(RefCell::new(value))
}

// --- symbols ----------------------------------------------------------------

/// An interned tag such as `.null` (§2, §5).
///
/// Symbols are compared by identity. The intern table holds *weak* entries, so
/// symbols minted from input data can be collected again — without that, a
/// program that decodes data in a loop grows without bound (§2).
#[derive(Clone)]
pub struct Sym(Rc<str>);

impl Sym {
    pub fn name(&self) -> &str {
        &self.0
    }

    pub fn ptr(&self) -> *const u8 {
        Rc::as_ptr(&self.0) as *const u8
    }
}

impl PartialEq for Sym {
    fn eq(&self, other: &Sym) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
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
        write!(f, ".{}", self.0)
    }
}

thread_local! {
    static INTERNED: RefCell<HashMap<Box<str>, Weak<str>>> = RefCell::new(HashMap::new());
    static SWEEP_COUNTDOWN: std::cell::Cell<u32> = const { std::cell::Cell::new(1024) };
}

/// Intern a symbol name. Equal names always give the identical symbol.
pub fn sym(name: &str) -> Sym {
    INTERNED.with(|table| {
        let mut table = table.borrow_mut();
        if let Some(weak) = table.get(name) {
            if let Some(strong) = weak.upgrade() {
                return Sym(strong);
            }
        }
        let strong: Rc<str> = Rc::from(name);
        table.insert(Box::from(name), Rc::downgrade(&strong));

        // Drop entries whose symbol has gone away. Amortised, so minting
        // symbols in a loop does not leave the table growing.
        let due = SWEEP_COUNTDOWN.with(|c| {
            let next = c.get().saturating_sub(1);
            c.set(if next == 0 { (table.len() as u32).max(1024) } else { next });
            next == 0
        });
        if due {
            table.retain(|_, weak| weak.strong_count() > 0);
        }
        Sym(strong)
    })
}

/// How many names the intern table is holding, for tests about collectability.
pub fn interned_count() -> usize {
    INTERNED.with(|t| {
        let mut t = t.borrow_mut();
        t.retain(|_, weak| weak.strong_count() > 0);
        t.len()
    })
}

thread_local! {
    static WELL_KNOWN: (Sym, Sym, Sym) = (sym("null"), sym("true"), sym("false"));
}

pub fn sym_null() -> Sym {
    WELL_KNOWN.with(|w| w.0.clone())
}

pub fn sym_true() -> Sym {
    WELL_KNOWN.with(|w| w.1.clone())
}

pub fn sym_false() -> Sym {
    WELL_KNOWN.with(|w| w.2.clone())
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
    /// It is a `Cell` and not a plain field so that marking a node shared never
    /// needs a mutable borrow: a deep comparison walks a structure holding
    /// immutable borrows, and reading through it copies as it goes.
    pub shared: StdCell<bool>,
}

/// Entries are kept in insertion order: a dict is compared key by key (§5), and
/// deterministic order keeps rendering and iteration reproducible.
#[derive(Debug)]
pub struct DictData {
    pub entries: Vec<(Sym, Value)>,
    pub shared: StdCell<bool>,
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

pub type ListRef = Rc<RefCell<ListData>>;
pub type DictRef = Rc<RefCell<DictData>>;

pub fn new_list(items: Vec<Value>) -> Value {
    Value::List(Rc::new(RefCell::new(ListData { items, shared: StdCell::new(false) })))
}

pub fn new_dict(entries: Vec<(Sym, Value)>) -> Value {
    Value::Dict(Rc::new(RefCell::new(DictData { entries, shared: StdCell::new(false) })))
}

// --- closures ---------------------------------------------------------------

/// A closure captures its enclosing **scope**, not a copy of it (§5).
pub struct Closure {
    pub name: String,
    pub params: Vec<Rc<str>>,
    pub chunk: Rc<Chunk>,
    pub scope: ScopeRef,
}

impl fmt::Debug for Closure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fn {}({})", self.name, self.params.join(", "))
    }
}

/// The language primitives. `alive()` is the only one (§9.5); everything else
/// in the spec's examples is a placeholder awaiting the standard library.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Native {
    Alive,
}

impl Native {
    pub fn name(self) -> &'static str {
        match self {
            Native::Alive => "alive",
        }
    }

    pub fn arity(self) -> usize {
        match self {
            Native::Alive => 0,
        }
    }
}

// --- references -------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum PathSeg {
    Key(Sym),
    Index(usize),
}

/// `&lvalue`: the root variable's cell plus the path from it (§5.1).
#[derive(Clone)]
pub struct RefValue {
    pub root: Cell,
    pub path: Rc<Vec<PathSeg>>,
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
    Str(Rc<str>),
    Sym(Sym),
    List(ListRef),
    Dict(DictRef),
    Fn(Rc<Closure>),
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

    /// `.null` and `.false` are falsy. *Everything else is truthy*, including
    /// `0`, `""` and `[]` (§5).
    pub fn truthy(&self) -> bool {
        match self {
            Value::Sym(s) => *s != sym_null() && *s != sym_false(),
            _ => true,
        }
    }

    pub fn as_num(&self, what: &str) -> Result<f64, Crash> {
        match self {
            Value::Num(n) => Ok(*n),
            other => Err(Crash::new(format!("{what} needs a number, got a {}", other.kind()))),
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
            rc.borrow().shared.set(true);
            Value::List(rc.clone())
        }
        Value::Dict(rc) => {
            rc.borrow().shared.set(true);
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
            let needs = rc.borrow().shared.get();
            if needs {
                let items: Vec<Value> = rc.borrow().items.iter().map(copy_value).collect();
                *rc = Rc::new(RefCell::new(ListData { items, shared: StdCell::new(false) }));
            }
        }
        Value::Dict(rc) => {
            let needs = rc.borrow().shared.get();
            if needs {
                let entries: Vec<(Sym, Value)> =
                    rc.borrow().entries.iter().map(|(k, v)| (k.clone(), copy_value(v))).collect();
                *rc = Rc::new(RefCell::new(DictData { entries, shared: StdCell::new(false) }));
            }
        }
        _ => {}
    }
}

// --- indexing ---------------------------------------------------------------

/// Turn an evaluated key into a path segment.
///
/// `d.k` is exactly `d[.k]` (§5), so there is one code path here and not two.
/// List indexing is 0-based and whole-numbers-only; §15.2 leaves the rest open,
/// see QUESTIONS.md §2.
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
            if *n < 0.0 {
                // QUESTIONS.md §2: negative indices do not wrap; indices start
                // at 0 and anything outside crashes like a missing key does.
                return Err(Crash::new(format!(
                    "list index {} is out of range: indices start at 0",
                    num_to_text(*n)
                )));
            }
            Ok(PathSeg::Index(*n as usize))
        }
        other => Err(Crash::new(format!(
            "a key must be a symbol and a list index a number, got a {}",
            other.kind()
        ))),
    }
}

// --- reading and writing places --------------------------------------------

/// Read one step: `container[key]`. Reading a missing key is a crash (§5).
pub fn get_member(container: &Value, key: &Value) -> Result<Value, Crash> {
    let container = deref(container)?;
    match (&container, path_segment(key)?) {
        (Value::Dict(rc), PathSeg::Key(s)) => match rc.borrow().get(&s) {
            Some(v) => read_through(v),
            None => Err(Crash::new(format!("no key .{} in this dict", s.name()))),
        },
        (Value::List(rc), PathSeg::Index(i)) => {
            let data = rc.borrow();
            match data.items.get(i) {
                Some(v) => read_through(v),
                None => Err(Crash::new(format!(
                    "list index {i} is out of range for a list of {} element(s)",
                    data.items.len()
                ))),
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
    let start = {
        let guard = root
            .try_borrow()
            .map_err(|_| Crash::new("a reference cycle was followed while reading"))?;
        guard.clone()
    };
    let mut current = match &start {
        Value::Ref(r) => {
            let mut full = r.path.as_ref().clone();
            full.extend_from_slice(path);
            return read_place(&r.root.clone(), &full);
        }
        other => other.clone(),
    };
    for seg in path {
        let key = match seg {
            PathSeg::Key(s) => Value::Sym(s.clone()),
            PathSeg::Index(i) => Value::Num(*i as f64),
        };
        current = get_member(&current, &key)?;
    }
    Ok(copy_value(&current))
}

/// Write `value` at `root` + `path`, path-copying shared nodes on the way (§5.1).
///
/// Writing a missing key **creates** it; reading one crashes (§5).
pub fn write_place(root: &Cell, path: &[PathSeg], value: Value) -> Result<(), Crash> {
    let mut guard = root
        .try_borrow_mut()
        .map_err(|_| Crash::new("a reference cycle was followed while assigning"))?;
    write_slot(&mut guard, path, value)
}

fn write_slot(slot: &mut Value, path: &[PathSeg], value: Value) -> Result<(), Crash> {
    // A reference in the slot is written *through*: `f(&a)` with `x = 5` in the
    // body sets the caller's `a` (see QUESTIONS.md §6).
    if let Value::Ref(r) = slot {
        let root = r.root.clone();
        let mut full = r.path.as_ref().clone();
        full.extend_from_slice(path);
        return write_place(&root, &full, value);
    }

    let Some(seg) = path.first() else {
        *slot = value;
        return Ok(());
    };

    unshare(slot);
    let last = path.len() == 1;
    match slot {
        Value::Dict(rc) => {
            let PathSeg::Key(key) = seg else {
                return Err(Crash::new("a dict is keyed by symbols, not by index"));
            };
            let mut data = rc
                .try_borrow_mut()
                .map_err(|_| Crash::new("a value that contains itself was assigned into"))?;
            match data.position(key) {
                Some(i) => write_slot(&mut data.entries[i].1, &path[1..], value),
                None if last => {
                    // Key writes create (§5).
                    data.entries.push((key.clone(), value));
                    Ok(())
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
            let mut data = rc
                .try_borrow_mut()
                .map_err(|_| Crash::new("a value that contains itself was assigned into"))?;
            let len = data.items.len();
            if *i >= len {
                return Err(Crash::new(format!(
                    "list index {i} is out of range for a list of {len} element(s)"
                )));
            }
            write_slot(&mut data.items[*i], &path[1..], value)
        }
        other => Err(Crash::new(format!("cannot assign into a {}", other.kind()))),
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
        (Value::List(x), Value::List(y)) => Rc::ptr_eq(x, y),
        (Value::Dict(x), Value::Dict(y)) => Rc::ptr_eq(x, y),
        (Value::Fn(x), Value::Fn(y)) => Rc::ptr_eq(x, y),
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

fn deep_equal_inner(a: &Value, b: &Value, visited: &mut Vec<(usize, usize)>, depth: u32) -> bool {
    if identical(a, b) {
        return true;
    }
    if matches!(a, Value::Ref(_)) || matches!(b, Value::Ref(_)) {
        let (Ok(a), Ok(b)) = (deref(a), deref(b)) else { return false };
        return deep_equal_inner(&a, &b, visited, depth);
    }
    match (a, b) {
        (Value::List(x), Value::List(y)) => {
            // The visited-pair set is only allocated once recursion passes a
            // small depth (§5).
            if depth > 8 {
                let pair = (Rc::as_ptr(x) as *const u8 as usize, Rc::as_ptr(y) as *const u8 as usize);
                if visited.contains(&pair) {
                    return true;
                }
                visited.push(pair);
            }
            let (x, y) = (x.borrow(), y.borrow());
            let equal = x.items.len() == y.items.len()
                && x.items
                    .iter()
                    .zip(y.items.iter())
                    .all(|(p, q)| deep_equal_inner(p, q, visited, depth + 1));
            if depth > 8 {
                visited.pop();
            }
            equal
        }
        (Value::Dict(x), Value::Dict(y)) => {
            if depth > 8 {
                let pair = (Rc::as_ptr(x) as *const u8 as usize, Rc::as_ptr(y) as *const u8 as usize);
                if visited.contains(&pair) {
                    return true;
                }
                visited.push(pair);
            }
            let (x, y) = (x.borrow(), y.borrow());
            let equal = x.entries.len() == y.entries.len()
                && x.entries.iter().all(|(k, v)| match y.get(k) {
                    Some(other) => deep_equal_inner(v, other, visited, depth + 1),
                    None => false,
                });
            if depth > 8 {
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

/// The text form of a value. See QUESTIONS.md §3: the spec does not define one,
/// but `+` on a string and a non-string forces the question.
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
            if crate::lexer::is_identifier(s.name()) {
                format!(".{}", s.name())
            } else {
                format!(".\"{}\"", crate::lexer::escape_string(s.name()))
            }
        }
        Value::List(rc) => {
            let data = rc.borrow();
            let items: Vec<String> = data.items.iter().map(to_text).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Dict(rc) => {
            let data = rc.borrow();
            if data.entries.is_empty() {
                return "{}".to_string();
            }
            let entries: Vec<String> = data
                .entries
                .iter()
                .map(|(k, v)| format!("{} : {}", to_text(&Value::Sym(k.clone())), to_text(v)))
                .collect();
            format!("{{ {} }}", entries.join(", "))
        }
        Value::Fn(c) => format!("fn {}({})", c.name, c.params.join(", ")),
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

    // `+` concatenates when either side is a string (QUESTIONS.md §3).
    if op == "+" && (matches!(a, Value::Str(_)) || matches!(b, Value::Str(_))) {
        return Ok(Value::Str(Rc::from(format!("{}{}", to_text(a), to_text(b)).as_str())));
    }

    match op {
        "+" | "-" | "*" | "/" | "%" | "<" | ">" | "<=" | ">=" => {
            let x = a.as_num(&format!("`{op}`"))?;
            let y = b.as_num(&format!("`{op}`"))?;
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
            let x = a.as_num(&format!("`{op}`"))?;
            let y = b.as_num(&format!("`{op}`"))?;
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
        "-" => Ok(Value::Num(-v.as_num("unary `-`")?)),
        "~" => Ok(int_result(!to_int32(v.as_num("`~`")?))),
        "not" => Ok(boolean(!v.truthy())),
        other => Err(Crash::new(format!("unknown operator `{other}`"))),
    }
}
