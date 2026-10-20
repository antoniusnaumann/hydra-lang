//! Compiling the tree to instructions.
//!
//! The spec's §10 checklist suggests a tree-walking evaluator. This uses a
//! stack machine instead, for one reason: a trail must be able to suspend
//! wherever it is — including inside a `parallel` block that a called function
//! opened — and a tree-walker cannot do that without either an OS thread per
//! trail or a hand-rolled stack. With explicit frames, "suspend this trail" is
//! just "stop stepping this task", and §9.5's *evaluate → check the cancel flag
//! → only then store* becomes a property of two instructions rather than a rule
//! to remember in twenty places.
//!
//! Nothing about the language changes: scopes are still hash maps with parent
//! pointers (§6), and names are still resolved dynamically — wherever anyone
//! could tell. Within one chunk, though, a name the chunk declared itself is
//! resolved here, to a slot in the frame:
//!
//! * A block that creates no closure and opens no `parallel` block cannot be
//!   seen by anything but its own code (see [`captures`]), so its names live
//!   in their slots and nowhere else.
//! * Any other block is a real scope, because a closure or a trail will look
//!   names up in it. Its names are still declared there, and the slot holds
//!   the same cell, so this chunk reaches the binding without a search.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::ast::*;
use crate::errors::{HydraError, Pos, Result};
use crate::text::Text;
use crate::value::{sym, BinOp, Native, Sym};

/// Where a place expression is rooted. Only a variable — possibly selected out
/// of a module — can root an assignment or a `&` (§5.1).
#[derive(Clone, Debug)]
pub enum Root {
    Name(Arc<str>),
    /// A name the compiler resolved to one of the frame's slots. `name` is for
    /// a slot that is still empty — a parameter whose default has not run —
    /// where the name means whatever it would have meant without the slot.
    Local { slot: u32, name: Arc<str> },
    Ns { module: Arc<str>, name: Arc<str> },
}

impl Root {
    pub fn describe(&self) -> String {
        match self {
            Root::Name(n) | Root::Local { name: n, .. } => n.to_string(),
            Root::Ns { module, name } => format!("{module}::{name}"),
        }
    }
}

/// What a call by name last learned about its name: that it meant the
/// builtin and nothing else, as of a given [`builtin_epoch`] and from a given
/// module (whose imports were part of the question). Zero is "nothing yet".
///
/// [`builtin_epoch`]: crate::value::builtin_epoch
#[derive(Default)]
pub struct CallCache(AtomicU64);

impl CallCache {
    fn key(epoch: u32, module: usize) -> u64 {
        (u64::from(epoch) << 32) | (module as u64 + 1)
    }

    pub fn holds(&self, epoch: u32, module: usize) -> bool {
        self.0.load(Ordering::Relaxed) == CallCache::key(epoch, module)
    }

    pub fn set(&self, epoch: u32, module: usize) {
        self.0.store(CallCache::key(epoch, module), Ordering::Relaxed);
    }
}

impl Clone for CallCache {
    fn clone(&self) -> CallCache {
        CallCache(AtomicU64::new(self.0.load(Ordering::Relaxed)))
    }
}

impl std::fmt::Debug for CallCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CallCache")
    }
}

/// What a statement's unconsumed result means where the statement stands
/// (§8.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unconsumed {
    /// File top level, including a trail of a top-level block: an ordinary
    /// value is printed, and `:reject` has nowhere to go.
    TopLevel,
    /// A function body: an ordinary value is dropped, and `:reject` returns
    /// from the function with the whole result.
    Function,
    /// A trail inside a function: dropped as in the function, but a trail
    /// cannot return from its function (§9.6), so `:reject` has nowhere to go.
    TrailInFunction,
}

#[derive(Clone, Debug)]
pub enum Instr {
    PushNum(f64),
    PushStr(Text),
    PushSym(Sym),
    MakeList(usize),
    /// Pops `n` key/value pairs. Keys are values rather than a static table
    /// because a key may be built at run time: `{ :"\(prefix)-id" : 1 }`.
    MakeDict(usize),
    /// Pops `n` values and concatenates their text forms: one `"…\(x)…"` (§1).
    Interpolate(usize),
    /// Pops a string and interns it as a symbol — the runtime half of a
    /// symbol built by interpolation (§2).
    MakeSym,
    MakeClosure { name: Arc<str>, chunk: Arc<Chunk> },
    Pop,
    /// The result of an expression written as a statement, which nothing
    /// consumed (§8.1). `spread` says the expression was a call, so the extras
    /// a call leaves behind are part of this result and not a stale answer.
    Unconsumed { spread: bool, at: Unconsumed },

    /// Read a variable: dereference a `&` transparently, then copy (§5.1).
    Load(Root),
    /// `x := v` — a fresh binding in the current scope, shadowing (§6). The
    /// slot gets the binding's cell too, which is how this chunk reaches it.
    /// `builtin` says a flat builtin has the name too, so that binding it
    /// changes what a call by that name can mean.
    Declare { name: Arc<str>, slot: u32, builtin: bool },
    /// `x := v` for a name that lives in a slot: a fresh binding there.
    DeclareLocal(u32),
    /// Empty the slots of a block that is being left, so what they held is
    /// released when the block ends rather than when the frame does.
    ClearLocals { from: u32, to: u32 },
    /// `place = v` — writes into the binding an outward search finds (§6),
    /// path-copying shared nodes and creating a missing final key (§5).
    Store { root: Root, segs: usize },
    /// The same store, for one target of a multi-value binding: the path is on
    /// top and the value sits *under* it, because the values arrived together
    /// and cannot be re-ordered around each target's path (channels §6.2).
    StoreUnder { root: Root, segs: usize },
    /// Spread a call's answer across `n` targets: the first value is on the
    /// stack and the rest are the frame's extras. Extras beyond `n` are
    /// dropped; naming more than arrived is a crash (channels §6.2).
    TakeValues(usize),
    /// `place += v` — reads the place, applies `op`, and writes the result back
    /// as **one** step, holding the lock the store takes for the read as well.
    /// A load and a store would let another trail write in between and lose the
    /// update (QUESTIONS.md §20).
    Update { root: Root, segs: usize, op: BinOp },
    /// `&place` (§5.1).
    MakeRef { root: Root, segs: usize },
    /// `a[k]`, which is also what `a.k` compiles to (§5).
    GetMember,
    /// `a[k]` for an `a` in a slot, with only `k` on the stack: the element
    /// is read where it is, without first copying `a` out to the stack.
    GetMemberLocal { slot: u32, name: Arc<str> },

    Binary(BinOp),
    Unary(&'static str),

    Jump(usize),
    JumpIfFalse(usize),
    /// Test an unconsumed loop-control value; consume it only on a match.
    JumpUnlessSignal { signal: &'static str, target: usize },
    /// `and`: falsy short-circuits and keeps its value.
    AndJump(usize),
    /// `or`: truthy short-circuits and keeps its value.
    OrJump(usize),

    /// Call the value on the stack under the arguments: one candidate only,
    /// because the callee was written as something other than a bare name.
    Call { positional: usize, names: Arc<Vec<Arc<str>>> },
    /// Call by name: every function bound to that name is a candidate, and the
    /// first that accepts the argument count and names is the one (§3).
    /// `locals` are the slots that hold the name, innermost first; they are
    /// candidates ahead of the scope chain. `native` is the flat builtin of
    /// that name, the last candidate there is. `cache` remembers that the
    /// name meant that builtin alone; `imports` that the module has no
    /// import of the name.
    CallName {
        name: Arc<str>,
        locals: Arc<[u32]>,
        native: Option<Native>,
        cache: CallCache,
        imports: CallCache,
        positional: usize,
        names: Arc<Vec<Arc<str>>>,
    },
    /// `return(a, b…)` as a statement in a function, with its `count`
    /// arguments on the stack: when `return` still means the builtin here,
    /// return them, as the call and its unconsumed result would. Otherwise
    /// fall through to the call, which is compiled right after.
    ReturnFast { count: usize, cache: CallCache },
    /// `receiver.name(…)`. The field wins where the receiver has one of that
    /// name holding something callable; otherwise the call is `name(receiver, …)`
    /// — the receiver becomes the first argument (§5.2).
    CallMethod { name: Arc<str>, locals: Arc<[u32]>, positional: usize, names: Arc<Vec<Arc<str>>> },
    /// `mod::name(…)`, and `x.mod::name(…)` with the receiver already first
    /// among the arguments. A qualified call falls through to nothing, but the
    /// module may have several candidates under the name, and resolution by
    /// shape picks between them (§7).
    /// `written` is `module::name` (or `::name`), for diagnostics.
    /// `natives` is what the builtin module of that name has under the name,
    /// which is what the call means wherever `module` is that builtin module
    /// — the usual case, and the one `cache` remembers.
    CallNs {
        module: Arc<str>,
        name: Arc<str>,
        written: Arc<str>,
        natives: Arc<[Native]>,
        cache: CallCache,
        positional: usize,
        names: Arc<Vec<Arc<str>>>,
    },
    /// Return `n` values, the first of which is the meaningful one and the rest
    /// additional information (channels §6.2).
    Return(usize),
    ReturnNull,

    PushScope,
    PopScope(usize),

    IterStart,
    /// Push the next element, or jump to `exit` when the list is spent.
    IterNext { exit: usize },
    IterDrop,

    /// A statement boundary: the scheduling point, and where a cancelled trail
    /// stops (§9.1, §9.5).
    Tick,

    /// Jump past a parameter's default when the call supplied that argument,
    /// which is when the parameter's slot is not empty. Defaults are evaluated
    /// in the *function's* scope, so a later one can refer to an earlier
    /// parameter.
    SkipIfLocal { slot: u32, target: usize },

    /// Open a `parallel` / `race` block.
    /// `arity` is how many trails the block will have, where that is known
    /// before it runs: the row form always, the spawning forms never. It is
    /// what lets a channel index that cannot exist crash (channels §6.7).
    BeginBlock { kind: BlockKind, label: Option<Arc<str>>, arity: Option<usize> },
    /// Start one trail of the open block. With `var`, the top of the stack
    /// becomes that binding in the trail's scope (`parallel for`).
    SpawnTrail { body: Arc<Chunk>, var: Option<Arc<str>>, column: usize },
    /// Wait for the open block: all trails, or the first (§9.3, §9.4).
    JoinBlock,
    /// Stop spawning after race completion or an unconsumed :break.
    JumpIfDecided(usize),

    /// End the current parallel-loop iteration (`:continue`).
    EndTrail,
    BreakParallel,
    /// `use fs`, `use fs as *`, `use fs as filesystem` (§7). `alias` is the
    /// name the module answers to when qualified, and `unqualified` is whether
    /// its names are bound bare as well.
    Use { module: Arc<str>, alias: Arc<str>, unqualified: bool },
}

/// What a call has to supply for one parameter.
#[derive(Clone, Debug)]
pub struct ParamInfo {
    /// Empty for the bare `*`, which binds nothing (channels §6.1).
    pub name: Arc<str>,
    /// `&name`: the argument must be a reference (§5.1).
    pub by_ref: bool,
    pub has_default: bool,
    /// `name*`: collects what is left of the positional arguments into a list.
    pub variadic: bool,
    /// Declared after the variadic, so only a named argument can fill it.
    pub keyword_only: bool,
    /// A flat builtin has the name too (see [`Instr::Declare`]).
    pub builtin: bool,
}

/// A compiled body: a function, a module, a trail, or a closure.
pub struct Chunk {
    pub name: String,
    pub file: Arc<str>,
    pub code: Vec<Instr>,
    /// One position per instruction, for crash diagnostics.
    pub pos: Vec<Pos>,
    /// Shared with every closure made from this chunk: a call reads them,
    /// and nothing ever changes them.
    pub params: Arc<[ParamInfo]>,
    /// How many slots a frame of this chunk needs.
    pub slots: usize,
    /// Parameter `i` is slot `i`. When this is set they live there alone and
    /// a call opens no scope for them; otherwise the slots hold the cells of
    /// the function's scope.
    pub flat: bool,
    /// How many positional arguments a call can bind by position alone:
    /// enough to reach the last parameter without a default, and no more
    /// than there are parameters. `None` with a variadic, which collects.
    pub positional: Option<(usize, usize)>,
    /// A parameter is `&`, so an argument has to be checked for one.
    pub by_ref: bool,
}

impl Chunk {
    /// How the function reads in a diagnostic — the same shape a closure's
    /// signature has, since it is built from the same parameters.
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

    pub fn required(&self) -> usize {
        self.params.iter().filter(|p| !p.has_default).count()
    }
}

impl std::fmt::Debug for Chunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Chunk({}, {} instrs)", self.name, self.code.len())
    }
}

/// The nearest local loop handler and its pending control jumps.
struct LoopCtx {
    /// A `for` loop owns an iterator that a `break` has to drop as it leaves.
    owns_iter: bool,
    scope_depth: usize,
    iter_depth: usize,
    /// The first slot inside the loop: leaving it empties everything above.
    slot_floor: u32,
    breaks: Vec<usize>,
    continues: Vec<usize>,
}

/// The names one block declared, in order. A name declared twice has two
/// entries, and the later one is what the name means from there on (§6).
struct Block {
    names: Vec<(Arc<str>, u32)>,
    /// The first slot this block owns.
    floor: u32,
    /// A real scope, which the names are declared into as well.
    scoped: bool,
}

pub struct Compiler {
    file: Arc<str>,
    code: Vec<Instr>,
    pos: Vec<Pos>,
    loops: Vec<LoopCtx>,
    scope_depth: usize,
    iter_depth: usize,
    /// True in a trail body, which has no function return handler (§9.6).
    in_trail: bool,
    /// True inside a function body, trails of its blocks included: that is
    /// where an unconsumed `:reject` has a call to hand back (§8.1).
    in_fn: bool,
    /// This chunk is an iteration body with an implicit parallel-loop handler.
    parallel_loop: bool,
    /// Open blocks, innermost last. A block without a scope only ever has
    /// blocks without one inside it.
    blocks: Vec<Block>,
    next_slot: u32,
    max_slots: u32,
    /// The function's parameters and top-level names are slots.
    flat: bool,
}

pub fn compile_program(program: &Program) -> Result<Arc<Chunk>> {
    let mut c = Compiler::new(&program.file, false);
    c.open_block(true);
    c.block(&program.body)?;
    c.emit(Instr::ReturnNull, Pos::NONE);
    Ok(Arc::new(c.finish(program.file.clone(), Vec::new())))
}

impl Compiler {
    fn new(file: &str, in_trail: bool) -> Compiler {
        Compiler {
            file: Arc::from(file),
            code: Vec::new(),
            pos: Vec::new(),
            loops: Vec::new(),
            scope_depth: 0,
            iter_depth: 0,
            in_trail,
            in_fn: false,
            parallel_loop: false,
            blocks: Vec::new(),
            next_slot: 0,
            max_slots: 0,
            flat: false,
        }
    }

    fn finish(self, name: String, params: Vec<ParamInfo>) -> Chunk {
        let required = params.iter().rposition(|p| !p.has_default).map_or(0, |i| i + 1);
        let positional = (!params.iter().any(|p| p.variadic)).then_some((required, params.len()));
        let by_ref = params.iter().any(|p| p.by_ref);
        Chunk {
            positional,
            by_ref,
            name,
            file: self.file,
            code: self.code,
            pos: self.pos,
            params: params.into(),
            slots: self.max_slots as usize,
            flat: self.flat,
        }
    }

    // --- slots --------------------------------------------------------------

    fn open_block(&mut self, scoped: bool) {
        self.blocks.push(Block { names: Vec::new(), floor: self.next_slot, scoped });
    }

    /// Close the innermost block, emptying the slots it declared.
    fn close_block(&mut self, pos: Pos) {
        let block = self.blocks.pop().expect("a block to close");
        if block.floor < self.next_slot {
            self.emit(Instr::ClearLocals { from: block.floor, to: self.next_slot }, pos);
        }
        self.next_slot = block.floor;
    }

    /// A fresh slot for `name` in the innermost block. Registered only now,
    /// after the value was compiled, so `x := x + 1` reads the old `x`.
    fn declare_slot(&mut self, name: &str) -> u32 {
        let slot = self.next_slot;
        self.next_slot += 1;
        self.max_slots = self.max_slots.max(self.next_slot);
        let block = self.blocks.last_mut().expect("an open block");
        if !name.is_empty() {
            block.names.push((Arc::from(name), slot));
        }
        slot
    }

    /// Bind the value on the stack to `name`: in a slot, and in the current
    /// scope as well where the block is one.
    fn declare(&mut self, name: &str, pos: Pos) {
        let scoped = self.blocks.last().expect("an open block").scoped;
        let slot = self.declare_slot(name);
        if scoped {
            let builtin = Native::lookup(name).is_some();
            self.emit(Instr::Declare { name: Arc::from(name), slot, builtin }, pos);
        } else {
            self.emit(Instr::DeclareLocal(slot), pos);
        }
    }

    /// Every slot `name` refers to, innermost first.
    fn slots_of(&self, name: &str) -> Vec<u32> {
        self.blocks
            .iter()
            .rev()
            .flat_map(|block| block.names.iter().rev())
            .filter(|(n, _)| n.as_ref() == name)
            .map(|(_, slot)| *slot)
            .collect()
    }

    fn root(&self, name: &str) -> Root {
        match self.slots_of(name).first() {
            Some(&slot) => Root::Local { slot, name: Arc::from(name) },
            None => Root::Name(Arc::from(name)),
        }
    }

    fn emit(&mut self, instr: Instr, pos: Pos) -> usize {
        self.code.push(instr);
        self.pos.push(pos);
        self.code.len() - 1
    }

    fn here(&self) -> usize {
        self.code.len()
    }

    fn patch(&mut self, at: usize, target: usize) {
        match &mut self.code[at] {
            Instr::Jump(t)
            | Instr::JumpUnlessSignal { target: t, .. }
            | Instr::JumpIfFalse(t)
            | Instr::AndJump(t)
            | Instr::OrJump(t)
            | Instr::IterNext { exit: t, .. }
            | Instr::JumpIfDecided(t)
            | Instr::SkipIfLocal { target: t, .. } => *t = target,
            other => panic!("cannot patch {other:?}"),
        }
    }

    fn binary(&self, op: &str, pos: Pos) -> Result<BinOp> {
        match BinOp::parse(op) {
            Some(op) => Ok(op),
            None => self.err(format!("unknown operator `{op}`"), pos),
        }
    }

    fn err<T>(&self, message: impl Into<String>, pos: Pos) -> Result<T> {
        Err(HydraError::new(message, &self.file, pos))
    }

    // --- statements ---------------------------------------------------------

    fn block(&mut self, body: &[Stmt]) -> Result<()> {
        for stmt in body {
            self.stmt(stmt)?;
        }
        Ok(())
    }

    /// A body that opens a scope of its own (§6).
    fn scoped_block(&mut self, body: &[Stmt], pos: Pos) -> Result<()> {
        let scoped = captures(body);
        if scoped {
            self.emit(Instr::PushScope, pos);
            self.scope_depth += 1;
        }
        self.open_block(scoped);
        self.block(body)?;
        self.close_block(pos);
        if scoped {
            self.scope_depth -= 1;
            self.emit(Instr::PopScope(1), pos);
        }
        Ok(())
    }

    fn stmt(&mut self, stmt: &Stmt) -> Result<()> {
        let pos = stmt.pos();
        self.emit(Instr::Tick, pos);
        match stmt {
            Stmt::Use { module, alias, unqualified, pos } => {
                self.emit(
                    Instr::Use {
                        module: Arc::from(module.as_str()),
                        alias: Arc::from(alias.as_deref().unwrap_or(module.as_str())),
                        unqualified: *unqualified,
                    },
                    *pos,
                );
            }
            Stmt::FnDecl { name, def, pos } => {
                self.closure(def, name, *pos)?;
                self.declare(name, *pos);
            }
            Stmt::Decl { names, value, pos } => {
                // A closure declared as `f := fn(…)` answers to `f` in
                // diagnostics; it has no name of its own otherwise.
                match (names.as_slice(), value) {
                    ([name], Expr::Closure(def)) => self.closure(def, name, def.pos)?,
                    (_, other) => self.expr(other)?,
                }
                if names.len() > 1 {
                    self.emit(Instr::TakeValues(names.len()), *pos);
                }
                // The last name's value is on top, so the names are bound from
                // the back.
                for name in names.iter().rev() {
                    // `_` names a value only to say it was seen (§8.1).
                    if is_discard_name(name) {
                        self.emit(Instr::Pop, *pos);
                        continue;
                    }
                    self.declare(name, *pos);
                }
            }
            Stmt::Assign { targets, op, value, pos } => {
                if let ([target], None) = (targets.as_slice(), &op) {
                    if is_discard(target) {
                        // `_ = f()` consumes the whole result and keeps none of
                        // it (§8.1).
                        self.expr(value)?;
                        self.emit(Instr::Pop, *pos);
                        return Ok(());
                    }
                }
                if let ([target], _) = (targets.as_slice(), &op) {
                    let (root, segs) = self.place(target)?;
                    self.expr(value)?;
                    match op {
                        None => self.emit(Instr::Store { root, segs }, *pos),
                        // The target's own path segments were evaluated once,
                        // above, and are shared by the read and the write —
                        // `a[next()] += 1` calls `next` once, like the
                        // `a[i] = a[i] + 1` it stands for and unlike the text
                        // of it.
                        Some(op) => {
                            let op = self.binary(op, *pos)?;
                            self.emit(Instr::Update { root, segs, op }, *pos)
                        }
                    };
                } else {
                    // Several targets for one call that returns several values.
                    // The values land together, so each target's own path is
                    // pushed *above* its value and `StoreUnder` reaches past it
                    // — which also means the paths are evaluated from the back.
                    self.expr(value)?;
                    self.emit(Instr::TakeValues(targets.len()), *pos);
                    for target in targets.iter().rev() {
                        if is_discard(target) {
                            self.emit(Instr::Pop, *pos);
                            continue;
                        }
                        let (root, segs) = self.place(target)?;
                        self.emit(Instr::StoreUnder { root, segs }, *pos);
                    }
                }
            }
            Stmt::Expr { expr, .. } => {
                let at = match (self.in_fn, self.in_trail) {
                    (false, _) => Unconsumed::TopLevel,
                    (true, false) => Unconsumed::Function,
                    (true, true) => Unconsumed::TrailInFunction,
                };
                match expr {
                    // `return(…)` in a function, where it most likely means
                    // the builtin: try returning straight away, and keep the
                    // call and its handler behind that for when it does not.
                    Expr::Call { callee, args, pos: call_pos, .. }
                        if at == Unconsumed::Function
                            && matches!(callee.as_ref(), Expr::Name { name, .. } if name == "return")
                            && args.iter().all(|a| a.name.is_none())
                            && self.slots_of("return").is_empty() =>
                    {
                        for arg in args {
                            self.expr(&arg.value)?;
                        }
                        let count = args.len();
                        self.emit(Instr::ReturnFast { count, cache: CallCache::default() }, *call_pos);
                        self.call_name("return", count, Arc::new(Vec::new()), *call_pos);
                    }
                    _ => self.expr(expr)?,
                }
                self.handle_loop_signals(pos)?;
                let spread = matches!(expr, Expr::Call { .. });
                self.emit(Instr::Unconsumed { spread, at }, pos);
            }
            Stmt::If { branches, pos, end_pos } => self.compile_if(branches, *pos, *end_pos)?,
            Stmt::While { cond, body, pos, .. } => {
                self.compile_while(cond, body, *pos)?
            }
            Stmt::For { var, iterable, body, pos, .. } => {
                self.compile_for(var, iterable, body, *pos)?
            }
            Stmt::Parallel { kind, trails, label, pos, .. } => {
                self.emit(
                    Instr::BeginBlock {
                        kind: *kind,
                        label: label.as_deref().map(Arc::from),
                        arity: Some(trails.len()),
                    },
                    *pos,
                );
                for trail in trails {
                    let chunk = self.trail_chunk(&trail.body, trail.column, false, None)?;
                    self.emit(
                        Instr::SpawnTrail { body: chunk, var: None, column: trail.column },
                        trail.pos,
                    );
                }
                self.emit(Instr::JoinBlock, *pos);
            }
            Stmt::ParallelFor { kind, var, iterable, body, label, pos, .. } => {
                self.emit(
                    Instr::BeginBlock {
                        kind: *kind,
                        label: label.as_deref().map(Arc::from),
                        arity: None,
                    },
                    *pos,
                );
                let chunk = self.trail_chunk(body, 0, true, Some(var))?;
                self.expr(iterable)?;
                self.emit(Instr::IterStart, *pos);
                self.iter_depth += 1;
                let top = self.here();
                let decided = self.emit(Instr::JumpIfDecided(0), *pos);
                let next = self.emit(Instr::IterNext { exit: 0 }, *pos);
                self.emit(
                    Instr::SpawnTrail { body: chunk, var: Some(Arc::from(var.as_str())), column: 0 },
                    *pos,
                );
                self.emit(Instr::Jump(top), *pos);
                let exit = self.here();
                self.patch(next, exit);
                self.patch(decided, exit);
                self.emit(Instr::IterDrop, *pos);
                self.iter_depth -= 1;
                self.emit(Instr::JoinBlock, *pos);
            }
            Stmt::ParallelWhile { kind, cond, body, label, pos, .. } => {
                self.emit(
                    Instr::BeginBlock {
                        kind: *kind,
                        label: label.as_deref().map(Arc::from),
                        arity: None,
                    },
                    *pos,
                );
                let chunk = self.trail_chunk(body, 0, true, None)?;
                let top = self.here();
                // Race completion or :break stops spawning before the next condition.
                let decided = self.emit(Instr::JumpIfDecided(0), *pos);
                self.expr(cond)?;
                let exit = self.emit(Instr::JumpIfFalse(0), *pos);
                self.emit(Instr::SpawnTrail { body: chunk, var: None, column: 0 }, *pos);
                self.emit(Instr::Jump(top), *pos);
                let after = self.here();
                self.patch(exit, after);
                self.patch(decided, after);
                self.emit(Instr::JoinBlock, *pos);
            }
        }
        Ok(())
    }

    fn compile_if(&mut self, branches: &[Branch], pos: Pos, end_pos: Pos) -> Result<()> {
        let mut done: Vec<usize> = Vec::new();
        for branch in branches {
            match &branch.cond {
                Some(cond) => {
                    self.expr(cond)?;
                    let skip = self.emit(Instr::JumpIfFalse(0), branch.pos);
                    self.scoped_block(&branch.body, branch.pos)?;
                    done.push(self.emit(Instr::Jump(0), branch.pos));
                    let next = self.here();
                    self.patch(skip, next);
                }
                None => {
                    self.scoped_block(&branch.body, branch.pos)?;
                }
            }
        }
        let end = self.here();
        for jump in done {
            self.patch(jump, end);
        }
        let _ = (pos, end_pos);
        Ok(())
    }

    fn compile_while(
        &mut self,
        cond: &Expr,
        body: &[Stmt],
        pos: Pos,
    ) -> Result<()> {
        let top = self.here();
        self.expr(cond)?;
        let exit = self.emit(Instr::JumpIfFalse(0), pos);

        self.loops.push(LoopCtx {
            owns_iter: false,
            scope_depth: self.scope_depth,
            iter_depth: self.iter_depth,
            slot_floor: self.next_slot,
            breaks: Vec::new(),
            continues: Vec::new(),
        });
        self.scoped_block(body, pos)?;
        self.emit(Instr::Jump(top), pos);

        let after = self.here();
        self.patch(exit, after);
        self.close_loop(after, top);
        Ok(())
    }

    fn compile_for(
        &mut self,
        var: &str,
        iterable: &Expr,
        body: &[Stmt],
        pos: Pos,
    ) -> Result<()> {
        self.expr(iterable)?;
        self.emit(Instr::IterStart, pos);
        self.iter_depth += 1;

        let top = self.here();
        let next = self.emit(Instr::IterNext { exit: 0 }, pos);

        self.loops.push(LoopCtx {
            owns_iter: true,
            scope_depth: self.scope_depth,
            iter_depth: self.iter_depth,
            slot_floor: self.next_slot,
            breaks: Vec::new(),
            continues: Vec::new(),
        });

        // The loop variable is declared in the body's own scope, fresh each
        // iteration, so a closure made in the body captures that iteration's
        // binding (§6).
        let scoped = captures(body);
        if scoped {
            self.emit(Instr::PushScope, pos);
            self.scope_depth += 1;
        }
        self.open_block(scoped);
        self.declare(var, pos);
        self.block(body)?;
        self.close_block(pos);
        if scoped {
            self.scope_depth -= 1;
            self.emit(Instr::PopScope(1), pos);
        }
        self.emit(Instr::Jump(top), pos);

        let exit = self.here();
        self.patch(next, exit);
        self.emit(Instr::IterDrop, pos);
        self.iter_depth -= 1;
        let after = self.here();
        self.close_loop(after, top);
        Ok(())
    }

    fn close_loop(&mut self, break_target: usize, continue_target: usize) {
        let ctx = self.loops.pop().expect("loop context");
        for jump in ctx.breaks {
            self.patch(jump, break_target);
        }
        for jump in ctx.continues {
            self.patch(jump, continue_target);
        }
    }

    fn handle_loop_signals(&mut self, pos: Pos) -> Result<()> {
        let nearest = self.loops.len().checked_sub(1);
        if nearest.is_none() && !self.parallel_loop {
            return Ok(());
        }
        for signal in ["break", "continue"] {
            let skip = self.emit(Instr::JumpUnlessSignal { signal, target: 0 }, pos);
            if let Some(index) = nearest {
                self.jump_out_of(index, pos, signal == "break")?;
            } else {
                self.emit(if signal == "break" { Instr::BreakParallel } else { Instr::EndTrail }, pos);
            }
            self.patch(skip, self.here());
        }
        Ok(())
    }

    /// Unwind scopes and iterators back to a loop, then jump.
    fn jump_out_of(&mut self, index: usize, pos: Pos, is_break: bool) -> Result<()> {
        let (scope_depth, iter_depth, owns_iter, slot_floor) = {
            let ctx = &self.loops[index];
            (ctx.scope_depth, ctx.iter_depth, ctx.owns_iter, ctx.slot_floor)
        };
        let scopes = self.scope_depth - scope_depth;
        if scopes > 0 {
            self.emit(Instr::PopScope(scopes), pos);
        }
        if slot_floor < self.next_slot {
            self.emit(Instr::ClearLocals { from: slot_floor, to: self.next_slot }, pos);
        }
        // `break` leaves the loop, so a `for`'s own iterator goes too;
        // `continue` jumps back to the top and keeps it.
        let mut iters = self.iter_depth - iter_depth;
        if is_break && owns_iter {
            iters += 1;
        }
        for _ in 0..iters {
            self.emit(Instr::IterDrop, pos);
        }
        let jump = self.emit(Instr::Jump(0), pos);
        if is_break {
            self.loops[index].breaks.push(jump);
        } else {
            self.loops[index].continues.push(jump);
        }
        Ok(())
    }

    // --- expressions --------------------------------------------------------

    fn expr(&mut self, expr: &Expr) -> Result<()> {
        match expr {
            Expr::Num { value, pos, .. } => {
                self.emit(Instr::PushNum(*value), *pos);
            }
            Expr::Str { parts, pos } => self.interpolated(parts, *pos)?,
            Expr::Sym(s) => self.symbol(s)?,
            Expr::List { items, pos } => {
                for item in items {
                    self.expr(item)?;
                }
                self.emit(Instr::MakeList(items.len()), *pos);
            }
            Expr::Dict { entries, pos } => {
                for (key, value) in entries {
                    self.symbol(key)?;
                    self.expr(value)?;
                }
                self.emit(Instr::MakeDict(entries.len()), *pos);
            }
            Expr::Name { name, pos } => {
                let root = self.root(name);
                self.emit(Instr::Load(root), *pos);
            }
            Expr::Namespace { module, name, pos } => {
                self.emit(
                    Instr::Load(Root::Ns {
                        module: Arc::from(module.as_str()),
                        name: Arc::from(name.as_str()),
                    }),
                    *pos,
                );
            }
            Expr::Method { name, pos, .. } => {
                return self.err(
                    format!(
                        "`.{name}` here is a call through a dot, and it is missing its `(…)`"
                    ),
                    *pos,
                )
            }
            Expr::Key { obj, key, pos } => {
                if let Some(root) = self.local_root(obj, key.is_static()) {
                    self.symbol(key)?;
                    self.emit(root, *pos);
                    return Ok(());
                }
                self.expr(obj)?;
                self.symbol(key)?;
                self.emit(Instr::GetMember, *pos);
            }
            Expr::Index { obj, index, pos } => {
                if let Some(root) = self.local_root(obj, is_pure(index)) {
                    self.expr(index)?;
                    self.emit(root, *pos);
                    return Ok(());
                }
                self.expr(obj)?;
                self.expr(index)?;
                self.emit(Instr::GetMember, *pos);
            }
            Expr::Call { callee, args, pos, .. } => {
                let names: Arc<Vec<Arc<str>>> = Arc::new(
                    args.iter().filter_map(|a| a.name.as_deref().map(Arc::from)).collect(),
                );
                let positional = args.iter().filter(|a| a.name.is_none()).count();
                match callee.as_ref() {
                    Expr::Name { name, .. } => {
                        for arg in args {
                            self.expr(&arg.value)?;
                        }
                        self.call_name(name, positional, names, *pos);
                    }
                    // `x.f(…)`: a field call, or a free function with `x` as
                    // its first argument (§5.2). Which one is a runtime
                    // question — it depends on what the receiver holds — so the
                    // whole decision goes into one instruction.
                    Expr::Method { obj, module: None, name, .. } => {
                        self.expr(obj)?;
                        for arg in args {
                            self.expr(&arg.value)?;
                        }
                        let locals = self.slots_of(name).into();
                        self.emit(
                            Instr::CallMethod {
                                name: Arc::from(name.as_str()),
                                locals,
                                positional,
                                names,
                            },
                            *pos,
                        );
                    }
                    // `x.mod::f(…)` is exactly `mod::f(x, …)`: a field cannot be
                    // namespaced, so there is nothing to decide (§7).
                    Expr::Method { obj, module: Some(module), name, .. } => {
                        self.expr(obj)?;
                        for arg in args {
                            self.expr(&arg.value)?;
                        }
                        self.emit(
                            Instr::CallNs {
                                module: Arc::from(module.as_str()),
                                name: Arc::from(name.as_str()),
                                written: Arc::from(format!("{module}::{name}")),
                                natives: ns_natives(module, name),
                                cache: CallCache::default(),
                                positional: positional + 1,
                                names,
                            },
                            *pos,
                        );
                    }
                    Expr::Namespace { module, name, .. } => {
                        for arg in args {
                            self.expr(&arg.value)?;
                        }
                        self.emit(
                            Instr::CallNs {
                                module: Arc::from(module.as_str()),
                                name: Arc::from(name.as_str()),
                                written: Arc::from(format!("{module}::{name}")),
                                natives: ns_natives(module, name),
                                cache: CallCache::default(),
                                positional,
                                names,
                            },
                            *pos,
                        );
                    }
                    _ => {
                        self.expr(callee)?;
                        for arg in args {
                            self.expr(&arg.value)?;
                        }
                        self.emit(Instr::Call { positional, names }, *pos);
                    }
                }
            }
            Expr::Unary { op, operand, pos } => {
                self.expr(operand)?;
                self.emit(Instr::Unary(op), *pos);
            }
            Expr::Ref { target, pos } => {
                let (root, segs) = self.place(target)?;
                self.emit(Instr::MakeRef { root, segs }, *pos);
            }
            Expr::Binary { op, left, right, pos } => match *op {
                "and" => {
                    self.expr(left)?;
                    let skip = self.emit(Instr::AndJump(0), *pos);
                    self.expr(right)?;
                    let end = self.here();
                    self.patch(skip, end);
                }
                "or" => {
                    self.expr(left)?;
                    let skip = self.emit(Instr::OrJump(0), *pos);
                    self.expr(right)?;
                    let end = self.here();
                    self.patch(skip, end);
                }
                op => {
                    self.expr(left)?;
                    self.expr(right)?;
                    let op = self.binary(op, *pos)?;
                    self.emit(Instr::Binary(op), *pos);
                }
            },
            Expr::Closure(def) => self.closure(def, "", def.pos)?,
        }
        Ok(())
    }

    /// The instruction that indexes `obj` in place, when `obj` is a name in a
    /// slot and the key cannot change it: the key is evaluated after `obj`
    /// is read, and a pure key is what makes that order unobservable.
    fn local_root(&self, obj: &Expr, pure_key: bool) -> Option<Instr> {
        let Expr::Name { name, .. } = obj else { return None };
        match self.root(name) {
            Root::Local { slot, name } if pure_key => Some(Instr::GetMemberLocal { slot, name }),
            _ => None,
        }
    }

    /// Call `name` with its arguments already on the stack.
    fn call_name(&mut self, name: &str, positional: usize, names: Arc<Vec<Arc<str>>>, pos: Pos) {
        let locals = self.slots_of(name).into();
        let instr = Instr::CallName {
            name: Arc::from(name),
            locals,
            native: Native::lookup(name),
            cache: CallCache::default(),
            imports: CallCache::default(),
            positional,
            names,
        };
        self.emit(instr, pos);
    }

    /// A string literal: one instruction when it is literal, a concatenation
    /// of its pieces when it interpolates (§1).
    fn interpolated(&mut self, parts: &[StrPart], pos: Pos) -> Result<()> {
        if parts.is_empty() {
            self.emit(Instr::PushStr(Text::new("")), pos);
            return Ok(());
        }
        if let [StrPart::Text(text)] = parts {
            self.emit(Instr::PushStr(Text::new(text)), pos);
            return Ok(());
        }
        for part in parts {
            match part {
                StrPart::Text(text) => {
                    self.emit(Instr::PushStr(Text::new(text)), pos);
                }
                StrPart::Expr(expr) => self.expr(expr)?,
            }
        }
        self.emit(Instr::Interpolate(parts.len()), pos);
        Ok(())
    }

    /// A symbol: interned at compile time, or built and interned at run time
    /// when it interpolates.
    fn symbol(&mut self, symbol: &SymLit) -> Result<()> {
        if symbol.is_static() {
            self.emit(Instr::PushSym(sym(&symbol.name)), symbol.pos);
            return Ok(());
        }
        self.interpolated(&symbol.parts, symbol.pos)?;
        self.emit(Instr::MakeSym, symbol.pos);
        Ok(())
    }

    fn closure(&mut self, def: &Arc<ClosureDef>, name: &str, pos: Pos) -> Result<()> {
        let mut sub = Compiler::new(&self.file, self.in_trail);
        sub.in_fn = true;
        // Parameter `i` is slot `i`, and the body's names follow. Where
        // nothing inside can capture the function's scope, nothing needs it
        // to exist, and the slots are all there is.
        sub.flat = !closure_captures(def);
        sub.open_block(!sub.flat);
        for param in &def.params {
            sub.declare_slot(&param.name);
        }
        // The prologue fills in the parameters the call did not supply.
        for (index, param) in def.params.iter().enumerate() {
            let Some(default) = &param.default else { continue };
            let slot = index as u32;
            let skip = sub.emit(Instr::SkipIfLocal { slot, target: 0 }, param.pos);
            sub.expr(default)?;
            if sub.flat {
                sub.emit(Instr::DeclareLocal(slot), param.pos);
            } else {
                let name = Arc::from(param.name.as_str());
                let builtin = Native::lookup(&param.name).is_some();
                sub.emit(Instr::Declare { name, slot, builtin }, param.pos);
            }
            let after = sub.here();
            sub.patch(skip, after);
        }
        match &def.body {
            ClosureBody::Expr(expr) => {
                sub.expr(expr)?;
                sub.emit(Instr::Return(1), def.pos);
            }
            ClosureBody::Block(body) => {
                // A function body is not a trail body: `return` is fine there,
                // and cancellation never interrupts a call (§9.5).
                sub.in_trail = false;
                sub.block(body)?;
                sub.emit(Instr::ReturnNull, def.end_pos);
            }
        }
        let params: Vec<ParamInfo> = def
            .params
            .iter()
            .map(|p| ParamInfo {
                name: Arc::from(p.name.as_str()),
                by_ref: p.by_ref,
                has_default: p.default.is_some(),
                variadic: p.variadic,
                keyword_only: p.keyword_only,
                builtin: Native::lookup(&p.name).is_some(),
            })
            .collect();
        let display = if name.is_empty() { "fn".to_string() } else { name.to_string() };
        let chunk = Arc::new(sub.finish(display, params));
        self.emit(Instr::MakeClosure { name: Arc::from(name), chunk }, pos);
        Ok(())
    }

    /// Compile a place expression: push its path segments, and return the root
    /// it is anchored at.
    fn place(&mut self, expr: &Expr) -> Result<(Root, usize)> {
        let mut segs: Vec<&Expr> = Vec::new();
        let mut current = expr;
        loop {
            match current {
                Expr::Name { name, .. } => {
                    let root = self.root(name);
                    return self.emit_segments(root, segs);
                }
                Expr::Namespace { module, name, pos } if module.is_empty() => {
                    return self.err(
                        format!("`::{name}` is a builtin, not a variable, so it cannot be assigned to or referenced"),
                        *pos,
                    )
                }
                Expr::Namespace { module, name, .. } => {
                    let root = Root::Ns {
                        module: Arc::from(module.as_str()),
                        name: Arc::from(name.as_str()),
                    };
                    return self.emit_segments(root, segs);
                }
                Expr::Key { obj, .. } | Expr::Index { obj, .. } => {
                    segs.push(current);
                    current = obj;
                }
                other => {
                    return self.err(
                        "only a variable, a dict key or a list element can be assigned to or referenced",
                        other.pos(),
                    )
                }
            }
        }
    }

    fn emit_segments(&mut self, root: Root, segs: Vec<&Expr>) -> Result<(Root, usize)> {
        let count = segs.len();
        for seg in segs.into_iter().rev() {
            match seg {
                Expr::Key { key, .. } => self.symbol(key)?,
                Expr::Index { index, .. } => {
                    self.expr(index)?;
                }
                _ => unreachable!("only keys and indices become path segments"),
            }
        }
        Ok((root, count))
    }

    /// Compile one trail body into its own chunk (§4, §9).
    /// `var` is the `parallel for` variable, which the trail starts with in
    /// its scope and in slot 0.
    fn trail_chunk(
        &mut self,
        body: &[Stmt],
        column: usize,
        parallel_loop: bool,
        var: Option<&str>,
    ) -> Result<Arc<Chunk>> {
        let mut sub = Compiler::new(&self.file, true);
        sub.in_fn = self.in_fn;
        sub.parallel_loop = parallel_loop;
        sub.open_block(true);
        if let Some(var) = var {
            sub.declare_slot(var);
        }
        sub.block(body)?;
        sub.emit(Instr::ReturnNull, Pos::NONE);
        Ok(Arc::new(sub.finish(format!("trail {column}"), Vec::new())))
    }
}

/// The builtins `module::name` names when `module` is a builtin module.
fn ns_natives(module: &str, name: &str) -> Arc<[Native]> {
    if module.is_empty() {
        return Native::lookup(name).into_iter().collect();
    }
    Native::in_module(module, name).into()
}

fn is_discard(target: &Expr) -> bool {
    matches!(target, Expr::Name { name, .. } if is_discard_name(name))
}

/// Whether evaluating `expr` can do nothing but compute a value: no call, so
/// no binding can change while it runs.
fn is_pure(expr: &Expr) -> bool {
    let parts = |parts: &[StrPart]| parts.iter().all(|p| matches!(p, StrPart::Text(_)) || matches!(p, StrPart::Expr(e) if is_pure(e)));
    match expr {
        Expr::Num { .. } | Expr::Name { .. } => true,
        Expr::Str { parts: p, .. } => parts(p),
        Expr::Sym(s) => parts(&s.parts),
        Expr::Unary { operand, .. } => is_pure(operand),
        Expr::Binary { left, right, .. } => is_pure(left) && is_pure(right),
        Expr::Index { obj, index, .. } => is_pure(obj) && is_pure(index),
        Expr::Key { obj, key, .. } => is_pure(obj) && parts(&key.parts),
        _ => false,
    }
}

/// Whether anything in `body` can see its scope after the code that declared
/// into it has moved on: a closure captures the scope chain it was made in,
/// and a trail reads and writes the one its block stands in (§6). A body
/// without either is only ever looked at by its own instructions, which is
/// what lets the compiler resolve its names to slots.
fn captures(body: &[Stmt]) -> bool {
    body.iter().any(stmt_captures)
}

fn closure_captures(def: &ClosureDef) -> bool {
    def.params.iter().any(|p| p.default.as_ref().is_some_and(expr_captures))
        || match &def.body {
            ClosureBody::Expr(expr) => expr_captures(expr),
            ClosureBody::Block(body) => captures(body),
        }
}

fn stmt_captures(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::FnDecl { .. }
        | Stmt::Parallel { .. }
        | Stmt::ParallelFor { .. }
        | Stmt::ParallelWhile { .. } => true,
        Stmt::Use { .. } => false,
        Stmt::Decl { value, .. } | Stmt::Expr { expr: value, .. } => expr_captures(value),
        Stmt::Assign { targets, value, .. } => {
            targets.iter().any(expr_captures) || expr_captures(value)
        }
        Stmt::If { branches, .. } => branches.iter().any(|b| {
            b.cond.as_ref().is_some_and(expr_captures) || captures(&b.body)
        }),
        Stmt::For { iterable, body, .. } => expr_captures(iterable) || captures(body),
        Stmt::While { cond, body, .. } => expr_captures(cond) || captures(body),
    }
}

fn expr_captures(expr: &Expr) -> bool {
    let parts = |parts: &[StrPart]| {
        parts.iter().any(|p| matches!(p, StrPart::Expr(e) if expr_captures(e)))
    };
    match expr {
        Expr::Closure(_) => true,
        Expr::Num { .. } | Expr::Name { .. } | Expr::Namespace { .. } => false,
        Expr::Str { parts: p, .. } => parts(p),
        Expr::Sym(s) => parts(&s.parts),
        Expr::List { items, .. } => items.iter().any(expr_captures),
        Expr::Dict { entries, .. } => {
            entries.iter().any(|(key, value)| parts(&key.parts) || expr_captures(value))
        }
        Expr::Key { obj, key, .. } => expr_captures(obj) || parts(&key.parts),
        Expr::Method { obj, .. } => expr_captures(obj),
        Expr::Index { obj, index, .. } => expr_captures(obj) || expr_captures(index),
        Expr::Call { callee, args, .. } => {
            expr_captures(callee) || args.iter().any(|a| expr_captures(&a.value))
        }
        Expr::Unary { operand, .. } => expr_captures(operand),
        Expr::Ref { target, .. } => expr_captures(target),
        Expr::Binary { left, right, .. } => expr_captures(left) || expr_captures(right),
    }
}
