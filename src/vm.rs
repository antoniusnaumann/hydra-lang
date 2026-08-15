//! The interpreter (spec §5–§10).
//!
//! One thread, one run queue, one task per trail. Instructions are stepped a
//! budget at a time; a task suspends by simply not being stepped again, which
//! is what makes a trail's suspension free at any depth.
//!
//! The two rules that shape everything here:
//!
//! * **Store after check** (§9.5). `Declare`, `Store` and `Update` ask whether
//!   the trail is still live *after* evaluating and *before* writing, so a
//!   cancelled trail's pending assignment simply does not happen.
//! * **A call is never interrupted** (§9.5). Cancellation is only noticed at a
//!   statement boundary of the trail's *own* body, so an in-flight call — and
//!   everything it invokes — runs to the end and no frame is abandoned.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};

use crate::ast::BlockKind;
use crate::compile::{compile_program, Chunk, Instr, Root};
use crate::errors::{Crash, HydraError, Pos, Site};
use crate::lexer::is_private;
use crate::parser::parse;
use crate::scope::{Scope, ScopeRef};
use crate::sched::{
    BlockCtx, BlockId, CancelFlag, Frame, IterState, Msg, OnReturn, ScopeSlot, Task, TaskId,
    TaskState, Waiter,
};
use crate::value::{
    binary_op, boolean, copy_value, deref, get_member, member_opt, new_dict, new_list, path_segment,
    push_place, read_place, sym, to_text, unary_op, update_place, write_place, Cell, Closure,
    Native, PathSeg, RefValue, Sym, Value, BUILTIN_MODULES,
};

#[derive(Clone, Debug)]
pub struct Options {
    /// Dead-trail crashes are fatal under strict mode, so tests fail on bugs
    /// that production would swallow (§9.5).
    pub strict: bool,
    /// Dead-trail crashes are reported on stderr by default; silenceable.
    pub report_dead_crashes: bool,
    /// How many statement boundaries a trail runs before the scheduler looks at
    /// the others. §9.1 asks for a preemption *check* at every statement
    /// boundary, which cancellation still does whatever this is set to; this
    /// only decides how often a trail is handed back to the queue.
    ///
    /// It defaults to 1 on a single worker, which gives the finest interleaving
    /// and a reproducible one. With a pool it defaults higher: a slice that
    /// short would spend more time in the scheduler's lock than in the program.
    pub step_budget: u32,
    /// How many OS threads run trails. Trails are still green threads — this is
    /// the cap on how many of them make progress at the same instant, and so on
    /// how much CPU-bound work a `parallel` block can actually overlap.
    ///
    /// Defaults to the machine's parallelism. Set it to 1 for a deterministic,
    /// reproducible interleaving, which is what the schedule-asserting tests do.
    pub threads: usize,
    pub search_path: Vec<PathBuf>,
}

impl Default for Options {
    fn default() -> Options {
        let search_path = std::env::var("HYDRA_PATH")
            .map(|v| v.split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect())
            .unwrap_or_default();
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        let step_budget = if threads > 1 { 64 } else { 1 };
        Options { strict: false, report_dead_crashes: true, step_budget, threads, search_path }
    }
}

pub struct ModuleRt {
    pub name: String,
    pub path: PathBuf,
    /// Replaced when the module body returns: declarations may have opened
    /// shadowing levels, so what it exports is the scope it *ended* with.
    pub scope: RwLock<ScopeRef>,
    /// Names `use` brought in, consulted after the lexical chain (§7).
    ///
    /// A name can have more than one: the most recent `use` wins for an
    /// unqualified *read*, and a *call* may fall through to an earlier one that
    /// accepts it (§3).
    pub imports: RwLock<HashMap<String, Vec<Cell>>>,
    /// `alias -> module`, for `mod::name`.
    pub aliases: RwLock<HashMap<String, usize>>,
    /// `alias -> builtin module`, for one the interpreter knows without a file
    /// (§7). A file of that name shadows it, so this is only consulted when no
    /// file answered.
    pub builtin_aliases: RwLock<HashMap<String, &'static str>>,
    /// Builtin modules a `use … as *` asked for by name, so unqualified lookup
    /// reaches them after the imports and before the flat builtins.
    pub star_builtins: RwLock<Vec<&'static str>>,
}

enum Flow {
    Next,
    Yield,
    Blocked,
    /// The trail is cancelled and stops here (§9.5).
    Stop,
    Done,
}

/// What the workers share: the task table, the run queue, and the count of
/// tasks currently being stepped.
///
/// One mutex covers all three so that "is there anything left to do" is a
/// single, exact question — the condition every worker parks on.
#[derive(Default)]
struct Sched {
    tasks: HashMap<TaskId, Task>,
    ready: VecDeque<TaskId>,
    running: usize,
    next_id: TaskId,
    /// Open `parallel` / `race` blocks, by id.
    blocks: HashMap<BlockId, BlockCtx>,
    next_block: BlockId,
    /// Tasks a block satisfied while they were still running. A worker parking
    /// one consumes its token instead of parking it, which is what closes the
    /// race between "I am about to block" and "your last child just finished".
    wakes: HashSet<TaskId>,
    /// What a parked channel call was answered with, waiting for the task to be
    /// stepped again (channels §6.5).
    deliveries: HashMap<TaskId, Vec<Value>>,
}

pub struct Vm {
    pub options: Options,
    modules: RwLock<Vec<Arc<ModuleRt>>>,
    module_by_path: Mutex<HashMap<PathBuf, usize>>,
    sched: Mutex<Sched>,
    /// Woken when a task becomes ready or the last one finishes.
    wake: Condvar,
    root_cancel: Arc<CancelFlag>,
    /// The first crash in a live trail: it ends the program (§8).
    crash: Mutex<Option<Crash>>,
    /// Crashes isolated to a dead trail (§9.5).
    dead_crashes: Mutex<Vec<Crash>>,
    /// The most trails ever stepping at the same moment: what "true
    /// parallelism" means, and what a test can assert on.
    peak_parallelism: AtomicUsize,
}

#[derive(Debug)]
pub struct RunResult {
    pub crash: Option<Crash>,
    pub dead_crashes: Vec<Crash>,
    pub root_scope: ScopeRef,
    /// The most trails that were stepping at once.
    pub peak_parallelism: usize,
}

impl RunResult {
    pub fn ok(&self) -> bool {
        self.crash.is_none()
    }
}

impl Vm {
    pub fn new(options: Options) -> Vm {
        Vm {
            options,
            modules: RwLock::new(Vec::new()),
            module_by_path: Mutex::new(HashMap::new()),
            sched: Mutex::new(Sched::default()),
            wake: Condvar::new(),
            root_cancel: CancelFlag::root(),
            crash: Mutex::new(None),
            dead_crashes: Mutex::new(Vec::new()),
            peak_parallelism: AtomicUsize::new(0),
        }
    }

    fn module(&self, id: usize) -> Arc<ModuleRt> {
        self.modules.read().unwrap_or_else(|e| e.into_inner())[id].clone()
    }

    fn new_module(&self, name: &str, path: PathBuf) -> usize {
        let mut modules = self.modules.write().unwrap_or_else(|e| e.into_inner());
        let id = modules.len();
        modules.push(Arc::new(ModuleRt {
            name: name.to_string(),
            path: path.clone(),
            scope: RwLock::new(Scope::root()),
            imports: RwLock::new(HashMap::new()),
            aliases: RwLock::new(HashMap::new()),
            builtin_aliases: RwLock::new(HashMap::new()),
            star_builtins: RwLock::new(Vec::new()),
        }));
        drop(modules);
        if !path.as_os_str().is_empty() {
            self.module_by_path.lock().unwrap_or_else(|e| e.into_inner()).insert(path, id);
        }
        id
    }

    /// Compile and run a source string as the main module.
    pub fn run_source(self: &Arc<Self>, src: &str, file: &str) -> Result<RunResult, HydraError> {
        let program = parse(src, file)?;
        let chunk = compile_program(&program)?;
        let path = PathBuf::from(file);
        let module = self.new_module(&stem(&path), path);
        self.spawn_root(chunk, module);
        Ok(self.run())
    }

    pub fn module_scope(&self, id: usize) -> ScopeRef {
        self.module(id).scope.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn spawn_root(&self, chunk: Arc<Chunk>, module: usize) {
        let scope = self.module_scope(module);
        let mut sched = self.sched.lock().unwrap_or_else(|e| e.into_inner());
        let id = sched.next_id;
        sched.next_id += 1;
        let task = Task {
            id,
            frames: vec![Frame {
                chunk,
                ip: 0,
                scopes: vec![ScopeSlot { scope, dynamic: false }],
                iters: Vec::new(),
                stack_base: 0,
                module,
                on_return: OnReturn::PushValue,
                call_site: Pos::NONE,
                is_module_body: true,
                provided: Vec::new(),
            }],
            stack: Vec::new(),
            cancel: self.root_cancel.clone(),
            block: None,
            blocks: Vec::new(),
            state: TaskState::Ready,
            base_depth: 1,
            is_trail: false,
            module,
            extras: Vec::new(),
            channel: None,
            delivery: None,
        };
        sched.tasks.insert(id, task);
        sched.ready.push_back(id);
    }

    /// Run until nothing is left to run.
    ///
    /// Trails are green threads spread over a pool of OS threads, so CPU-bound
    /// work in a `parallel` block runs on as many cores as the pool has. The
    /// pool size is the cap on true parallelism; the number of *trails* is not
    /// capped (§9.1).
    ///
    /// Orphaned losers of a `race` are still in the queue, so the program waits
    /// for them at exit (§9.4's recommendation).
    pub fn run(self: &Arc<Self>) -> RunResult {
        let workers = self.options.threads.max(1);
        let extra: Vec<_> = (1..workers)
            .map(|_| {
                let vm = Arc::clone(self);
                std::thread::spawn(move || vm.work())
            })
            .collect();
        // The calling thread is a worker too, so a one-thread run needs no
        // threads at all and keeps a schedule a test can assert on.
        self.work();
        for worker in extra {
            let _ = worker.join();
        }

        RunResult {
            crash: self.crash.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            dead_crashes: self.dead_crashes.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            root_scope: self.module_scope(0),
            peak_parallelism: self.peak_parallelism.load(Ordering::Relaxed),
        }
    }

    /// One worker: take a ready trail, step it for a slice, hand it back.
    fn work(&self) {
        loop {
            let mut task = {
                let mut sched = self.sched.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    match sched.ready.pop_front() {
                        Some(id) => match sched.tasks.remove(&id) {
                            Some(mut task) => {
                                if let Some(values) = sched.deliveries.remove(&id) {
                                    task.delivery = Some(values);
                                }
                                sched.running += 1;
                                self.peak_parallelism.fetch_max(sched.running, Ordering::Relaxed);
                                break task;
                            }
                            None => continue,
                        },
                        // Nothing ready and nothing running means nothing can
                        // become ready: the program is over for every worker.
                        None if sched.running == 0 => {
                            self.wake.notify_all();
                            return;
                        }
                        None => {
                            sched = self.wake.wait(sched).unwrap_or_else(|e| e.into_inner());
                        }
                    }
                }
            };

            let id = task.id;
            let outcome = self.run_slice(&mut task);

            let mut sched = self.sched.lock().unwrap_or_else(|e| e.into_inner());
            sched.running -= 1;
            let was_ready = sched.ready.len();
            match outcome {
                Ok(Flow::Yield) => {
                    sched.tasks.insert(id, task);
                    sched.ready.push_back(id);
                }
                Ok(Flow::Blocked) => {
                    if sched.wakes.remove(&id) {
                        sched.ready.push_back(id);
                    } else {
                        task.state = TaskState::Blocked;
                    }
                    sched.tasks.insert(id, task);
                }
                Ok(Flow::Done) | Ok(Flow::Stop) => self.finish_task(&mut sched, task, Ok(())),
                Ok(Flow::Next) => unreachable!("a slice never ends mid-step"),
                Err(crash) => self.finish_task(&mut sched, task, Err(crash)),
            }
            // Wake a parked worker only when there is more to take than before,
            // or when this was the last one running and everyone should leave.
            let gained = sched.ready.len() > was_ready;
            let finished = sched.ready.is_empty() && sched.running == 0;
            drop(sched);
            if finished {
                self.wake.notify_all();
            } else if gained {
                self.wake.notify_one();
            }
        }
    }

    fn run_slice(&self, task: &mut Task) -> Result<Flow, Crash> {
        // A parked channel call left its answer here; the instruction that
        // asked for it is already behind us (channels §6.5).
        if let Some(values) = task.delivery.take() {
            let mut answered = values.into_iter();
            let value = answered.next().unwrap_or_else(Value::null);
            task.extras = answered.collect();
            task.push(value);
        }
        let mut budget = self.options.step_budget;
        // The running chunk is held for the whole slice and refreshed only when
        // a call or a return changes frames. Cloning it per instruction would
        // put an atomic refcount on a chunk every worker shares.
        let mut chunk = match task.frames.last() {
            Some(frame) => frame.chunk.clone(),
            None => return Ok(Flow::Done),
        };
        loop {
            if task.frames.is_empty() {
                return Ok(Flow::Done);
            }
            if !Arc::ptr_eq(&chunk, &task.frame().chunk) {
                chunk = task.frame().chunk.clone();
            }
            match self.step(task, &chunk, &mut budget) {
                Ok(Flow::Next) => continue,
                Ok(other) => return Ok(other),
                Err(crash) => return Err(self.decorate(task, crash)),
            }
        }
    }

    /// Point a crash at the statement that raised it and at the frames below.
    fn decorate(&self, task: &Task, mut crash: Crash) -> Crash {
        if !crash.site.pos.is_known() {
            if let Some(frame) = task.frames.last() {
                let pos = frame.chunk.pos.get(frame.ip.saturating_sub(1)).copied().unwrap_or(Pos::NONE);
                crash.site = Site::new(frame.chunk.file.to_string(), pos);
            }
        }
        // A frame records where it was called *from*, which is a position in
        // the frame below it, so that is the file to name.
        for i in (1..task.frames.len()).rev() {
            let frame = &task.frames[i];
            if frame.call_site.is_known() {
                let caller = &task.frames[i - 1];
                crash.trace.push(Site::new(caller.chunk.file.to_string(), frame.call_site));
            }
        }
        crash
    }

    fn step(&self, task: &mut Task, chunk: &Chunk, budget: &mut u32) -> Result<Flow, Crash> {
        // Falling off the end of a body returns nothing.
        const END: Instr = Instr::ReturnNull;
        let instr = {
            let frame = task.frame_mut();
            if frame.ip >= chunk.code.len() {
                &END
            } else {
                let at = frame.ip;
                frame.ip += 1;
                &chunk.code[at]
            }
        };

        match instr {
            Instr::PushNum(n) => task.push(Value::Num(*n)),
            Instr::PushStr(s) => task.push(Value::Str(s.clone())),
            Instr::PushSym(s) => task.push(Value::Sym(s.clone())),
            Instr::Pop => {
                task.pop();
            }
            Instr::MakeList(n) => {
                let at = task.stack.len() - *n;
                let items: Vec<Value> = task.stack.split_off(at);
                task.push(new_list(items));
            }
            Instr::MakeDict(n) => {
                let n = *n;
                let at = task.stack.len() - n * 2;
                let flat: Vec<Value> = task.stack.split_off(at);
                let mut entries: Vec<(Sym, Value)> = Vec::with_capacity(n);
                let mut pairs = flat.into_iter();
                while let (Some(key), Some(value)) = (pairs.next(), pairs.next()) {
                    let Value::Sym(key) = key else {
                        return Err(Crash::new(format!(
                            "a dict key must be a symbol, got a {}",
                            key.kind()
                        )));
                    };
                    // A later duplicate wins; `check` reports the literal.
                    match entries.iter().position(|(k, _)| *k == key) {
                        Some(i) => entries[i].1 = value,
                        None => entries.push((key, value)),
                    }
                }
                task.push(new_dict(entries));
            }
            Instr::Interpolate(n) => {
                let at = task.stack.len() - *n;
                let parts: Vec<Value> = task.stack.split_off(at);
                let mut text = String::new();
                for part in &parts {
                    text.push_str(&to_text(part));
                }
                task.push(Value::Str(Arc::from(text.as_str())));
            }
            Instr::MakeSym => {
                let value = task.pop();
                match &value {
                    Value::Str(text) => task.push(Value::Sym(sym(text))),
                    other => {
                        return Err(Crash::new(format!(
                            "a symbol is built from a string, got a {}",
                            other.kind()
                        )))
                    }
                }
            }
            Instr::MakeClosure { name, params, chunk } => {
                let scope = task.scope().clone();
                task.push(Value::Fn(Arc::new(Closure {
                    name: name.to_string(),
                    params: params.as_ref().clone(),
                    chunk: chunk.clone(),
                    scope,
                })));
            }

            Instr::Load(root) => {
                let value = self.load(task, root)?;
                task.push(value);
            }
            Instr::Declare(name) => {
                let value = task.pop();
                if task.should_stop() {
                    return Ok(Flow::Stop);
                }
                // Redeclaring a name already bound in this scope is a *fresh*
                // binding, and a closure that captured the old one must keep
                // it (§6), so the new binding goes in a level of its own.
                if task.scope().get_local(name).is_some() {
                    let child = Scope::child(task.scope());
                    task.frame_mut().push_scope(child, true);
                }
                task.scope().declare(name, value);
            }
            Instr::Store { root, segs } => {
                let value = task.pop();
                let path = self.take_path(task, *segs)?;
                // Evaluate → check the cancel flag → only then store (§9.5).
                if task.should_stop() {
                    return Ok(Flow::Stop);
                }
                let cell = self.cell_for(task, root)?;
                write_place(&cell, &path, value)?;
            }
            Instr::StoreUnder { root, segs } => {
                // The values arrived together, so this target's path sits on
                // top of the value rather than under it (channels §6.2).
                let path = self.take_path(task, *segs)?;
                let value = task.pop();
                // Evaluate → check the cancel flag → only then store (§9.5).
                if task.should_stop() {
                    return Ok(Flow::Stop);
                }
                let cell = self.cell_for(task, root)?;
                write_place(&cell, &path, value)?;
            }
            Instr::TakeValues(count) => {
                let value = task.pop();
                let extras = std::mem::take(&mut task.extras);
                let answered = 1 + extras.len();
                if *count > answered {
                    return Err(Crash::new(format!(
                        "this call answers with {answered} value{}, but {count} were named",
                        if answered == 1 { "" } else { "s" }
                    )));
                }
                task.push(value);
                for extra in extras.into_iter().take(count - 1) {
                    task.push(extra);
                }
            }
            Instr::Update { root, segs, op } => {
                let operand = task.pop();
                let path = self.take_path(task, *segs)?;
                // Evaluate → check the cancel flag → only then store (§9.5).
                // A cancelled trail does not read its target either: the whole
                // read-modify-write is the pending assignment.
                if task.should_stop() {
                    return Ok(Flow::Stop);
                }
                let cell = self.cell_for(task, root)?;
                update_place(&cell, &path, op, &operand)?;
            }
            Instr::MakeRef { root, segs } => {
                let path = self.take_path(task, *segs)?;
                let cell = self.cell_for(task, root)?;
                task.push(Value::Ref(RefValue { root: cell, path: Arc::new(path) }));
            }
            Instr::GetMember => {
                let key = task.pop();
                let obj = task.pop();
                let value = get_member(&obj, &key)?;
                task.push(value);
            }

            Instr::Binary(op) => {
                let right = task.pop();
                let left = task.pop();
                task.push(binary_op(op, &left, &right)?);
            }
            Instr::Unary(op) => {
                let value = task.pop();
                task.push(unary_op(op, &value)?);
            }

            Instr::Jump(target) => {
                let target = *target;
                if target <= task.frame().ip {
                    // A backward jump is a loop iteration: bill it, so a loop
                    // whose body has no statements still yields (§9.1).
                    if *budget == 0 {
                        task.frame_mut().ip = target;
                        return Ok(Flow::Yield);
                    }
                    *budget -= 1;
                }
                task.frame_mut().ip = target;
            }
            Instr::JumpIfFalse(target) => {
                let value = task.pop();
                if !value.truthy() {
                    task.frame_mut().ip = *target;
                }
            }
            Instr::AndJump(target) => {
                let keep = !task.stack.last().expect("operand").truthy();
                if keep {
                    task.frame_mut().ip = *target;
                } else {
                    task.pop();
                }
            }
            Instr::OrJump(target) => {
                let keep = task.stack.last().expect("operand").truthy();
                if keep {
                    task.frame_mut().ip = *target;
                } else {
                    task.pop();
                }
            }

            Instr::Call { positional, names } => {
                let args = take_args(task, *positional, names);
                let callee = task.pop();
                let Some(bound) = bind_args(&callee, &args) else {
                    return Err(rejected(None, &[callee], &args));
                };
                return self.enter(task, callee, bound);
            }
            Instr::CallName { name, positional, names } => {
                let args = take_args(task, *positional, names);
                // Every function bound to the name is a candidate, innermost
                // first; the first that accepts the call is the one (§3).
                let candidates = self.candidates(task, name)?;
                if candidates.is_empty() {
                    return Err(Crash::new(format!("`{name}` is not declared")));
                }
                // A concrete arity is tried before any variadic, whichever is
                // nearer: a `*` accepts everything positional, and would
                // otherwise swallow every narrower candidate behind it
                // (channels §6.1).
                let take = |variadic: bool| {
                    candidates
                        .iter()
                        .filter(|c| is_variadic(c) == variadic)
                        .find_map(|c| bind_args(c, &args).map(|bound| (c.clone(), bound)))
                };
                let chosen = take(false).or_else(|| take(true));
                let Some((callee, bound)) = chosen else {
                    return Err(rejected(Some(name), &candidates, &args));
                };
                return self.enter(task, callee, bound);
            }
            Instr::CallNs { module, name, positional, names } => {
                let args = take_args(task, *positional, names);
                let candidates = self.ns_candidates(task, module, name)?;
                let take = |variadic: bool| {
                    candidates
                        .iter()
                        .filter(|c| is_variadic(c) == variadic)
                        .find_map(|c| bind_args(c, &args).map(|bound| (c.clone(), bound)))
                };
                let Some((callee, bound)) = take(false).or_else(|| take(true)) else {
                    let written = if module.is_empty() {
                        format!("::{name}")
                    } else {
                        format!("{module}::{name}")
                    };
                    return Err(rejected(Some(&written), &candidates, &args));
                };
                return self.enter(task, callee, bound);
            }
            Instr::CallMethod { name, positional, names } => {
                let mut args = take_args(task, *positional, names);
                let receiver = task.pop();
                // A field of that name wins, but only if it holds something
                // callable: a number named `count` is not what `x.count()`
                // meant (§5.2).
                // Only a dict has fields, and asking a number for one is not
                // an error here — it is the question that decides which call
                // this is.
                let field = match deref(&receiver)? {
                    Value::Dict(_) => match member_opt(&receiver, &Value::Sym(sym(name)))? {
                        Some(value) if param_specs(&value).is_some() => Some(value),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(callee) = field {
                    // A field call takes no receiver, so a `&` in front of one
                    // marks something nothing will be handed. Fields win (§5.2),
                    // so this is the marker being wrong rather than the call.
                    if matches!(receiver, Value::Ref(_)) {
                        return Err(Crash::new(format!(
                            "`.{name}` is a field holding a function, and a field call is \
                             handed no receiver: there is nothing for the `&` to mark"
                        )));
                    }
                    let Some(bound) = bind_args(&callee, &args) else {
                        return Err(rejected(Some(name), &[callee], &args));
                    };
                    return self.enter(task, callee, bound);
                }

                // Otherwise the receiver is the first argument, and the rest of
                // the call resolves exactly as a written-out one does (§3).
                args.positional.insert(0, receiver);
                let candidates = self.candidates(task, name)?;
                if candidates.is_empty() {
                    return Err(Crash::new(format!(
                        "no field `.{name}` and no function `{name}`: \
                         `x.{name}(…)` is either"
                    )));
                }
                let take = |variadic: bool| {
                    candidates
                        .iter()
                        .filter(|c| is_variadic(c) == variadic)
                        .find_map(|c| bind_args(c, &args).map(|bound| (c.clone(), bound)))
                };
                let Some((callee, bound)) = take(false).or_else(|| take(true)) else {
                    return Err(rejected(Some(name), &candidates, &args));
                };
                return self.enter(task, callee, bound);
            }
            Instr::Return(count) => {
                let at = task.stack.len() - count;
                let values: Vec<Value> = task.stack.split_off(at);
                return Ok(self.pop_frame(task, values));
            }
            Instr::ReturnNull => {
                return Ok(self.pop_frame(task, vec![Value::null()]));
            }

            Instr::PushScope => {
                let child = Scope::child(task.scope());
                task.frame_mut().push_scope(child, false);
            }
            Instr::PopScope(n) => task.frame_mut().pop_scopes(*n),

            Instr::IterStart => {
                let value = deref(&task.pop())?;
                match value {
                    Value::List(list) => {
                        list.read()
                            .unwrap_or_else(|e| e.into_inner())
                            .shared
                            .store(true, Ordering::Relaxed);
                        task.frame_mut().iters.push(IterState { list, index: 0 });
                    }
                    other => {
                        return Err(Crash::new(format!(
                            "`for … in` needs a list, got a {}",
                            other.kind()
                        )))
                    }
                }
            }
            Instr::IterNext { exit } => {
                let next = {
                    let iter = task.frame_mut().iters.last_mut().expect("iterator");
                    let data = iter.list.read().unwrap_or_else(|e| e.into_inner());
                    if iter.index < data.items.len() {
                        let value = data.items[iter.index].clone();
                        iter.index += 1;
                        Some(value)
                    } else {
                        None
                    }
                };
                match next {
                    Some(value) => {
                        let value = match &value {
                            Value::Ref(r) => read_place(&r.root, &r.path)?,
                            other => copy_value(other),
                        };
                        task.push(value);
                    }
                    None => task.frame_mut().ip = *exit,
                }
            }
            Instr::IterDrop => {
                task.frame_mut().iters.pop();
            }

            Instr::SkipIfProvided { index, target } => {
                if task.frame().provided.get(*index) == Some(&true) {
                    task.frame_mut().ip = *target;
                }
            }
            Instr::Tick => {
                // A statement boundary: where a cancelled trail stops, and the
                // scheduling point (§9.1, §9.5).
                if task.should_stop() {
                    return Ok(Flow::Stop);
                }
                if *budget == 0 {
                    task.frame_mut().ip -= 1;
                    return Ok(Flow::Yield);
                }
                *budget -= 1;
            }

            Instr::BeginBlock { kind, arity, .. } => {
                let mut sched = self.sched.lock().unwrap_or_else(|e| e.into_inner());
                let id = sched.next_block;
                sched.next_block += 1;
                sched.blocks.insert(
                    id,
                    BlockCtx {
                        kind: *kind,
                        owner: task.id,
                        children: Vec::new(),
                        flags: Vec::new(),
                        pending: 0,
                        decided: false,
                        arity: *arity,
                        spawn_done: false,
                        finished: Vec::new(),
                        mail: VecDeque::new(),
                        waiting: Vec::new(),
                    },
                );
                task.blocks.push(id);
            }
            Instr::SpawnTrail { body, var, column } => {
                let init = var.clone().map(|name| (name, task.pop()));
                self.spawn_trail(task, body.clone(), init, *column);
            }
            Instr::JumpIfDecided(target) => {
                let decided = match task.blocks.last() {
                    Some(id) => {
                        let sched = self.sched.lock().unwrap_or_else(|e| e.into_inner());
                        sched.blocks.get(id).map(|b| b.decided).unwrap_or(true)
                    }
                    None => false,
                };
                if decided {
                    task.frame_mut().ip = *target;
                }
            }
            Instr::JoinBlock => {
                let satisfied = match task.blocks.last() {
                    Some(id) => {
                        let mut sched = self.sched.lock().unwrap_or_else(|e| e.into_inner());
                        // No further trail can appear from here, which is what
                        // makes "nobody is left to receive" a final answer
                        // rather than a guess (channels §1).
                        if let Some(block) = sched.blocks.get_mut(id) {
                            if !block.spawn_done {
                                block.spawn_done = true;
                                let woken = closed_waiters(block);
                                deliver_all(&mut sched, woken);
                            }
                        }
                        match sched.blocks.get(id) {
                            // Satisfied: drop the block, so a loser finishing
                            // later finds nothing to report to (§9.4).
                            Some(block) if block.satisfied() => {
                                sched.blocks.remove(id);
                                true
                            }
                            Some(_) => false,
                            None => true,
                        }
                    }
                    None => true,
                };
                if satisfied {
                    task.blocks.pop();
                } else {
                    task.frame_mut().ip -= 1;
                    return Ok(Flow::Blocked);
                }
            }

            Instr::EndTrail => return Ok(Flow::Stop),
            Instr::Use { module, alias, unqualified } => {
                return self.use_module(task, module, alias, *unqualified)
            }
        }
        Ok(Flow::Next)
    }

    // --- calls and frames ---------------------------------------------------

    /// Every function the name could mean, innermost binding first, then the
    /// most recent `use` first, then the builtin (§3, §7).
    fn candidates(&self, task: &Task, name: &str) -> Result<Vec<Value>, Crash> {
        let mut cells: Vec<Cell> = task.scope().all_bindings(name);
        let module = task.frame().module;
        if let Some(imported) =
            self.module(module).imports.read().unwrap_or_else(|e| e.into_inner()).get(name)
        {
            cells.extend(imported.iter().cloned());
        }
        let mut out = Vec::new();
        for cell in cells {
            let value = cell.read().unwrap_or_else(|e| e.into_inner()).clone();
            out.push(match value {
                Value::Ref(r) => read_place(&r.root, &r.path)?,
                other => copy_value(&other),
            });
        }
        // A `use fs as *` puts the module's builtins here — after the imports
        // and before the flat ones, which is where §7 says an import goes.
        for module in
            self.module(module).star_builtins.read().unwrap_or_else(|e| e.into_inner()).iter()
        {
            out.extend(Native::in_module(module, name).into_iter().map(Value::Native));
        }
        if let Some(native) = Native::lookup(name) {
            out.push(Value::Native(native));
        }
        Ok(out)
    }

    /// Enter a call whose arguments are already matched to its parameters.
    fn enter(
        &self,
        task: &mut Task,
        callee: Value,
        bound: Vec<Option<Value>>,
    ) -> Result<Flow, Crash> {
        let specs = param_specs(&callee).expect("a callee that bound its arguments");
        // `&name` in the signature says the caller must mark it, and the caller
        // is the only one who can (§5.1). This is checked *after* resolution,
        // deliberately: a missing `&` is an error to report, not a reason to
        // quietly pick a different function.
        for (spec, value) in specs.iter().zip(bound.iter()) {
            if spec.by_ref {
                match value {
                    Some(Value::Ref(_)) => {}
                    Some(_) => {
                        return Err(Crash::new(format!(
                            "`{}` takes `{}` by reference: write `&…` at the call site, \
                             or it is handed a copy",
                            signature_of(&callee),
                            spec.name
                        )))
                    }
                    None => {}
                }
            }
        }

        match callee {
            Value::Fn(closure) => {
                let scope = Scope::child(&closure.scope);
                let mut provided = vec![false; closure.params.len()];
                for (i, (param, value)) in closure.params.iter().zip(bound).enumerate() {
                    if let Some(value) = value {
                        // The bare `*` has no name and binds nothing: it only
                        // closes the positional list (channels §6.1).
                        if !param.name.is_empty() {
                            scope.declare(&param.name, value);
                        }
                        provided[i] = true;
                    }
                }
                let call_site = {
                    let frame = task.frame();
                    frame.chunk.pos.get(frame.ip.saturating_sub(1)).copied().unwrap_or(Pos::NONE)
                };
                let module = task.frame().module;
                let stack_base = task.stack.len();
                task.frames.push(Frame {
                    chunk: closure.chunk.clone(),
                    ip: 0,
                    scopes: vec![ScopeSlot { scope, dynamic: false }],
                    iters: Vec::new(),
                    stack_base,
                    module,
                    on_return: OnReturn::PushValue,
                    call_site,
                    is_module_body: false,
                    provided,
                });
                Ok(Flow::Next)
            }
            Value::Native(native) => {
                let out = match native {
                    Native::Send => self.send(task, &bound)?,
                    Native::Receive => self.receive(task, &bound)?,
                    Native::Channel => {
                        let (_, me) = self.in_trail(task, "channel")?;
                        NativeOut::Values(vec![index_of(me)])
                    }
                    other => NativeOut::Values(self.native(task, other, bound)?),
                };
                match out {
                    NativeOut::Values(values) => {
                        let mut answered = values.into_iter();
                        let value = answered.next().unwrap_or_else(Value::null);
                        task.extras = answered.collect();
                        task.push(value);
                        Ok(Flow::Next)
                    }
                    // The trail is parked; what it answers arrives later.
                    NativeOut::Blocked => Ok(Flow::Blocked),
                }
            }
            other => Err(Crash::new(format!("cannot call a {}", other.kind()))),
        }
    }

    /// The builtins of `spec/hydra_stdlib.md`, plus `alive()` (§9.5).
    fn native(
        &self,
        task: &Task,
        native: Native,
        args: Vec<Option<Value>>,
    ) -> Result<Vec<Value>, Crash> {
        let arg = |i: usize| args.get(i).cloned().flatten().unwrap_or_else(Value::null);
        match native {
            // Dynamic, no token threading, `.true` outside any trail — and
            // false during crash shutdown too (§9.5).
            Native::Alive => Ok(vec![boolean(!task.cancel.is_cancelled())]),
            Native::Print => {
                let end = match args.get(1).cloned().flatten() {
                    Some(end) => to_text(&deref(&end)?),
                    None => "\n".to_string(),
                };
                let mut out = std::io::stdout().lock();
                // One `print` is one write, so two trails interleave by line
                // and never mid-line.
                let _ = write!(out, "{}{end}", to_text(&deref(&arg(0))?));
                let _ = out.flush();
                Ok(vec![Value::null()])
            }
            Native::Has => Ok(vec![boolean(member_opt(&arg(0), &arg(1))?.is_some())]),
            Native::Get => Ok(vec![member_opt(&arg(0), &arg(1))?.unwrap_or_else(|| arg(2))]),
            Native::Len => {
                let value = deref(&arg(0))?;
                let len = match &value {
                    Value::List(rc) => rc.read().unwrap_or_else(|e| e.into_inner()).items.len(),
                    Value::Dict(rc) => rc.read().unwrap_or_else(|e| e.into_inner()).entries.len(),
                    // Characters, not bytes: source is UTF-8 (§1).
                    Value::Str(text) => text.chars().count(),
                    other => {
                        return Err(Crash::new(format!(
                            "`len` counts a list, a dict or a string, got a {}",
                            other.kind()
                        )))
                    }
                };
                Ok(vec![Value::Num(len as f64)])
            }
            Native::Send | Native::Receive | Native::Channel => {
                unreachable!("the channel calls are dispatched before this")
            }
            Native::Push => {
                let Value::Ref(target) = arg(0) else {
                    unreachable!("checked by the by-reference rule above")
                };
                let len = push_place(&target.root, &target.path, arg(1))?;
                Ok(vec![Value::Num(len as f64)])
            }
            // A module's builtins live with the module (spec/hydra_fs.md).
            other => crate::fs::call(other, &args),
        }
    }

    /// Return from a frame with everything it answered: the first value is the
    /// meaningful one and the rest are additional information, held until the
    /// binding site names them or the next call replaces them (channels §6.2).
    // --- auto-channels (spec/hydra_channels.md) ------------------------------

    /// `send(value, to*, mode = .wait)`.
    ///
    /// Returns `.true` when the value reached someone or was buffered for
    /// someone, and `.false` when every trail it could have reached has already
    /// ended. `.wait` parks until one of those two is true.
    fn send(&self, task: &mut Task, args: &[Option<Value>]) -> Result<NativeOut, Crash> {
        let (block_id, me) = self.in_trail(task, "send")?;
        let value = args.first().cloned().flatten().unwrap_or_else(Value::null);
        let to = channel_list(args.get(1).cloned().flatten())?;
        let mode = match args.get(2).cloned().flatten() {
            Some(mode) => send_mode(&mode)?,
            None => SendMode::Wait,
        };

        let mut sched = self.sched.lock().unwrap_or_else(|e| e.into_inner());
        let Some(block) = sched.blocks.get_mut(&block_id) else {
            // The block is gone: a `race` released control and this trail is an
            // orphan, so there is nobody left to reach.
            return Ok(NativeOut::Values(vec![boolean(false)]));
        };
        check_indices(block, me, to.as_ref())?;

        let targets: Vec<usize> = match &to {
            Some(indices) => indices.clone(),
            None => (0..block.children.len()).filter(|i| *i != me).collect(),
        };

        let mut woken: Vec<(TaskId, Vec<Value>)> = Vec::new();
        let mut delivered = false;

        match mode {
            SendMode::Broadcast => {
                // One copy for every eligible trail: those parked take it now,
                // the rest find it waiting (channels §5).
                for target in targets {
                    if block.is_finished(target) {
                        continue;
                    }
                    delivered = true;
                    match take_waiter(block, target, me) {
                        Some(waiter) => woken.push((waiter, vec![copy_value(&value), index_of(me)])),
                        None => block.mail.push_back(Msg {
                            from: me,
                            to: Some(vec![target]),
                            value: copy_value(&value),
                            waiter: None,
                        }),
                    }
                }
            }
            SendMode::Detach | SendMode::Wait => {
                match take_any_waiter(block, &targets, me) {
                    Some(waiter) => {
                        woken.push((waiter, vec![value, index_of(me)]));
                        delivered = true;
                    }
                    None if !block.any_peer_left(me, to.as_ref()) => {}
                    None => {
                        delivered = true;
                        let waiter = match mode {
                            SendMode::Wait => Some(task.id),
                            _ => None,
                        };
                        block.mail.push_back(Msg { from: me, to: to.clone(), value, waiter });
                        if mode == SendMode::Wait {
                            let parked = park_check(block);
                            deliver_all(&mut sched, woken);
                            parked?;
                            return Ok(NativeOut::Blocked);
                        }
                    }
                }
            }
        }
        deliver_all(&mut sched, woken);
        Ok(NativeOut::Values(vec![boolean(delivered)]))
    }

    /// `receive(from*)` — the value, and the trail that sent it. Both come back
    /// `.null` when no eligible sender is left, which is the one answer a
    /// sender cannot fake (channels §1).
    fn receive(&self, task: &mut Task, args: &[Option<Value>]) -> Result<NativeOut, Crash> {
        let (block_id, me) = self.in_trail(task, "receive")?;
        let from = channel_list(args.first().cloned().flatten())?;

        let mut sched = self.sched.lock().unwrap_or_else(|e| e.into_inner());
        let Some(block) = sched.blocks.get_mut(&block_id) else {
            return Ok(NativeOut::Values(closed()));
        };
        check_indices(block, me, from.as_ref())?;

        if let Some(at) = block.mail.iter().position(|msg| {
            BlockCtx::addressed_to(msg, me)
                && from.as_ref().is_none_or(|only| only.contains(&msg.from))
        }) {
            let msg = block.mail.remove(at).expect("the message just found");
            // A `.wait` sender was parked behind its value.
            if let Some(sender) = msg.waiter {
                deliver_all(&mut sched, vec![(sender, vec![boolean(true)])]);
            }
            return Ok(NativeOut::Values(vec![msg.value, index_of(msg.from)]));
        }

        if !block.any_peer_left(me, from.as_ref()) {
            return Ok(NativeOut::Values(closed()));
        }

        block.waiting.push(Waiter { task: task.id, channel: me, from });
        park_check(block)?;
        Ok(NativeOut::Blocked)
    }

    /// The block a channel call belongs to, and this trail's own index in it.
    fn in_trail(&self, task: &Task, what: &str) -> Result<(BlockId, usize), Crash> {
        match (task.block, task.channel) {
            (Some(block), Some(channel)) => Ok((block, channel)),
            _ => Err(Crash::new(format!(
                "`{what}` names a trail's siblings, and there is no trail here: \
                 it belongs inside a `parallel` or `race` block"
            ))),
        }
    }

    fn pop_frame(&self, task: &mut Task, values: Vec<Value>) -> Flow {
        let frame = task.frames.pop().expect("a frame to return from");
        task.stack.truncate(frame.stack_base);
        if frame.is_module_body {
            // Declarations may have opened shadowing levels, so the namespace
            // a module exports is the scope it *ended* with (§6, §7).
            *self.module(frame.module).scope.write().unwrap_or_else(|e| e.into_inner()) =
                frame.scope().clone();
        }
        match frame.on_return {
            OnReturn::PushValue => {
                if task.frames.is_empty() {
                    return Flow::Done;
                }
                let mut answered = values.into_iter();
                let value = answered.next().unwrap_or_else(Value::null);
                task.extras = answered.collect();
                task.push(value);
            }
            OnReturn::BindModule { name, alias, unqualified, module } => {
                self.bind_module(task, module, &name, &alias, unqualified);
            }
        }
        if task.frames.is_empty() {
            Flow::Done
        } else {
            Flow::Next
        }
    }

    // --- places -------------------------------------------------------------

    fn take_path(&self, task: &mut Task, segs: usize) -> Result<Vec<PathSeg>, Crash> {
        let at = task.stack.len() - segs;
        let raw: Vec<Value> = task.stack.split_off(at);
        raw.iter().map(path_segment).collect()
    }

    fn cell_for(&self, task: &Task, root: &Root) -> Result<Cell, Crash> {
        match root {
            Root::Name(name) => self.lookup(task, name).ok_or_else(|| {
                // `=` to a name with no binding anywhere crashes (§6).
                Crash::new(format!("`{name}` is not declared; use `:=` to declare it"))
            }),
            Root::Ns { module, name } => self.lookup_ns(task, module, name),
        }
    }

    fn lookup(&self, task: &Task, name: &str) -> Option<Cell> {
        if let Some(cell) = task.scope().lookup(name) {
            return Some(cell);
        }
        let module = task.frame().module;
        let module = self.module(module);
        let imports = module.imports.read().unwrap_or_else(|e| e.into_inner());
        imports.get(name).and_then(|c| c.first().cloned())
    }

    /// What a builtin module has under a name, for the importer's own aliases.
    fn builtin_candidates(&self, task: &Task, module: &str, name: &str) -> Vec<Native> {
        let importer = self.module(task.frame().module);
        let target = importer
            .builtin_aliases
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(module)
            .copied();
        match target {
            Some(builtin) => Native::in_module(builtin, name),
            None => Vec::new(),
        }
    }

    /// Every function a qualified name could mean.
    ///
    /// A qualified call does not fall through to anything else (§7), but the
    /// module itself may have more than one candidate under the name — which is
    /// how `fs::read` offers both `read(path)` and `read(path, fallback)`.
    fn ns_candidates(&self, task: &Task, module: &str, name: &str) -> Result<Vec<Value>, Crash> {
        if module.is_empty() {
            return match Native::lookup(name) {
                Some(native) => Ok(vec![Value::Native(native)]),
                None => Err(Crash::new(format!("there is no builtin named `{name}`"))),
            };
        }
        if is_private(name) {
            return Err(Crash::new(format!(
                "`{name}` is private to module `{module}` and cannot be selected"
            )));
        }
        let importer = task.frame().module;
        let target = self
            .module(importer)
            .aliases
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(module)
            .copied();
        if let Some(target) = target {
            let scope = self.module_scope(target);
            let cells = scope.all_bindings(name);
            if cells.is_empty() {
                return Err(Crash::new(format!("module `{module}` has no name `{name}`")));
            }
            let mut out = Vec::new();
            for cell in cells {
                let value = cell.read().unwrap_or_else(|e| e.into_inner()).clone();
                out.push(match value {
                    Value::Ref(r) => read_place(&r.root, &r.path)?,
                    other => copy_value(&other),
                });
            }
            return Ok(out);
        }
        let builtins = self.builtin_candidates(task, module, name);
        if builtins.is_empty() {
            let known = self
                .module(importer)
                .builtin_aliases
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(module);
            return Err(if known {
                Crash::new(format!("module `{module}` has no name `{name}`"))
            } else {
                Crash::new(format!("no module `{module}` is in scope; `use {module}` first"))
            });
        }
        Ok(builtins.into_iter().map(Value::Native).collect())
    }

    fn lookup_ns(&self, task: &Task, module: &str, name: &str) -> Result<Cell, Crash> {
        if module.is_empty() {
            return Err(Crash::new(format!("`::{name}` is a builtin, not a variable")));
        }
        let importer = task.frame().module;
        let target =
            self.module(importer).aliases.read().unwrap_or_else(|e| e.into_inner()).get(module).copied();
        let Some(target) = target else {
            // A builtin module has no scope to select from, so a name of one is
            // a value only through the call that names it.
            if !self.builtin_candidates(task, module, name).is_empty() {
                return Err(Crash::new(format!(
                    "`{module}::{name}` is a builtin: it can be called, but not passed around"
                )));
            }
            return Err(Crash::new(format!("no module `{module}` is in scope; `use {module}` first")));
        };
        // Private names are not reachable through `::` (§7).
        if is_private(name) {
            return Err(Crash::new(format!(
                "`{name}` is private to module `{module}` and cannot be selected"
            )));
        }
        self.module_scope(target)
            .get_local(name)
            .ok_or_else(|| Crash::new(format!("module `{module}` has no name `{name}`")))
    }

    fn load(&self, task: &Task, root: &Root) -> Result<Value, Crash> {
        let cell = match root {
            Root::Name(name) => match self.lookup(task, name) {
                Some(cell) => cell,
                // A builtin is a global name consulted *after* the scope
                // chain, so a program can shadow one.
                None => match Native::lookup(name) {
                    Some(native) => return Ok(Value::Native(native)),
                    None => return Err(Crash::new(format!("`{name}` is not declared"))),
                },
            },
            // `::name` is the builtin, whatever else holds the name (§7).
            Root::Ns { module, name } if module.is_empty() => {
                return match Native::lookup(name) {
                    Some(native) => Ok(Value::Native(native)),
                    None => Err(Crash::new(format!("there is no builtin named `{name}`"))),
                }
            }
            Root::Ns { module, name } => self.lookup_ns(task, module, name)?,
        };
        let value = cell.read().unwrap_or_else(|e| e.into_inner()).clone();
        match value {
            Value::Ref(r) => read_place(&r.root, &r.path),
            other => Ok(copy_value(&other)),
        }
    }

    // --- trails -------------------------------------------------------------

    fn spawn_trail(
        &self,
        task: &mut Task,
        body: Arc<Chunk>,
        init: Option<(Arc<str>, Value)>,
        _column: usize,
    ) {
        // A trail's scope's parent is the block's enclosing scope, so the trail
        // reads and writes the parent's bindings while everything it declares
        // with `:=` stays local to it (§6).
        let scope = Scope::child(task.scope());
        if let Some((name, value)) = init {
            scope.declare(&name, value);
        }
        let mut sched = self.sched.lock().unwrap_or_else(|e| e.into_inner());
        let id = sched.next_id;
        sched.next_id += 1;
        let block = task.blocks.last().copied();
        // Trail 0 is the first spawned and the number climbs, which is the
        // index a sibling addresses and `channel()` answers. For the row form
        // that is the column; for the spawning forms it is spawn order, which
        // is all there is (channels §7.1).
        let channel = block
            .and_then(|b| sched.blocks.get(&b))
            .map(|block| block.children.len())
            .unwrap_or(0);
        let cancel = CancelFlag::child(&task.cancel);
        let child = Task {
            id,
            frames: vec![Frame {
                chunk: body,
                ip: 0,
                scopes: vec![ScopeSlot { scope, dynamic: false }],
                iters: Vec::new(),
                stack_base: 0,
                module: task.frame().module,
                on_return: OnReturn::PushValue,
                call_site: Pos::NONE,
                is_module_body: false,
                provided: Vec::new(),
            }],
            stack: Vec::new(),
            cancel: cancel.clone(),
            block,
            blocks: Vec::new(),
            state: TaskState::Ready,
            base_depth: 1,
            is_trail: true,
            module: task.module,
            extras: Vec::new(),
            channel: Some(channel),
            delivery: None,
        };
        if let Some(block) = block.and_then(|b| sched.blocks.get_mut(&b)) {
            block.children.push(id);
            block.flags.push(cancel);
            block.pending += 1;
        }
        sched.tasks.insert(id, child);
        sched.ready.push_back(id);
        drop(sched);
        // A parked worker may be the one that runs it.
        self.wake.notify_one();
    }

    /// Retire a finished trail and wake whoever was waiting on it.
    ///
    /// The scheduler lock is already held, so the parent's bookkeeping, the
    /// sibling cancellations and the wake-up all happen as one step — no other
    /// worker can see the block half-decided.
    fn finish_task(&self, sched: &mut Sched, task: Task, result: Result<(), Crash>) {
        if let Err(crash) = result {
            if task.cancel.is_cancelled() {
                // A crash inside a dead trail is isolated: that trail ends, the
                // program continues (§9.5).
                if self.options.report_dead_crashes {
                    eprintln!("hydra: crash in a cancelled trail (isolated): {crash}");
                }
                self.dead_crashes.lock().unwrap_or_else(|e| e.into_inner()).push(crash.clone());
                if self.options.strict {
                    self.fatal(crash);
                }
            } else {
                self.fatal(crash);
            }
        }

        // The block may already be gone: a `race` releases control at its first
        // completion and its losers finish afterwards, with nothing to report to.
        let Some(block_id) = task.block else { return };
        let Some(block) = sched.blocks.get_mut(&block_id) else { return };

        block.pending -= 1;
        if let Some(channel) = task.channel {
            block.finished.push(channel);
        }
        let mut to_cancel: Vec<TaskId> = Vec::new();
        if block.kind == BlockKind::Race && !block.decided {
            block.decided = true;
            // Losers are cancelled (§9.4); they are never interrupted, so they
            // stop at their own next statement boundary. The flag comes from
            // the block, because a sibling that is running at this moment is
            // not in the task table and would otherwise escape.
            for (index, id) in block.children.iter().enumerate() {
                if *id != task.id {
                    to_cancel.push(*id);
                    block.flags[index].cancel();
                }
            }
        }
        let owner = block.owner;
        let satisfied = block.satisfied();

        // Whoever was parked on this trail has to hear that it will not answer.
        let mut woken = closed_waiters(block);
        // A cancelled trail parked on a channel is woken too: it gets the same
        // answer as a closed channel and then runs no further statement. The
        // flag is already set above, so it sees itself dead the moment it wakes
        // — this is the one place §9.5's "never interrupted" bends, and it
        // bends where nothing can observe it (channels §6.5).
        for id in &to_cancel {
            woken.extend(cancel_waiter(block, *id));
        }
        deliver_all(sched, woken);

        if !satisfied {
            return;
        }
        // The owner may be blocked, or still running on another worker and not
        // in the table at all. Leaving a token covers both.
        match sched.tasks.get_mut(&owner) {
            Some(parent) if parent.state == TaskState::Blocked => {
                parent.state = TaskState::Ready;
                sched.ready.push_back(owner);
            }
            _ => {
                sched.wakes.insert(owner);
            }
        }
    }

    /// A crash in a live trail: mark every sibling cancelled, keep the first
    /// diagnostic, and let the program drain (§8).
    fn fatal(&self, crash: Crash) {
        let mut first = self.crash.lock().unwrap_or_else(|e| e.into_inner());
        if first.is_none() {
            *first = Some(crash);
        }
        self.root_cancel.cancel();
    }

    // --- modules (§7) -------------------------------------------------------

    fn use_module(
        &self,
        task: &mut Task,
        name: &str,
        alias: &str,
        unqualified: bool,
    ) -> Result<Flow, Crash> {
        let importer = task.frame().module;
        let from = self.module(importer).path.clone();
        let Some(path) = self.resolve_module(name, &from) else {
            // A file of that name would have shadowed it, which is why naming
            // one after a builtin module is discouraged (§7).
            if let Some(builtin) = BUILTIN_MODULES.iter().find(|m| **m == name) {
                self.bind_builtin(task, builtin, alias, unqualified);
                return Ok(Flow::Next);
            }
            return Err(Crash::new(format!(
                "cannot find module `{name}`: no `{name}.hy` beside {} or on HYDRA_PATH, \
                 and no builtin module of that name",
                from.display()
            )));
        };

        let known = self.module_by_path.lock().unwrap_or_else(|e| e.into_inner()).get(&path).copied();
        if let Some(id) = known {
            // Executing is skipped if the file is already in scope, but binding
            // always runs — including for a circular import, which resolves to
            // whatever is bound so far (§7).
            self.bind_module(task, id, name, alias, unqualified);
            return Ok(Flow::Next);
        }

        let src = std::fs::read_to_string(&path)
            .map_err(|e| Crash::new(format!("cannot read {}: {e}", path.display())))?;
        let file = path.display().to_string();
        let program = parse(&src, &file).map_err(|e| Crash::new(e.to_string()))?;
        let chunk = compile_program(&program).map_err(|e| Crash::new(e.to_string()))?;
        let id = self.new_module(name, path);
        let scope = self.module_scope(id);
        let stack_base = task.stack.len();
        task.frames.push(Frame {
            chunk,
            ip: 0,
            scopes: vec![ScopeSlot { scope, dynamic: false }],
            iters: Vec::new(),
            stack_base,
            module: id,
            on_return: OnReturn::BindModule {
                name: Arc::from(name),
                alias: Arc::from(alias),
                unqualified,
                module: id,
            },
            call_site: Pos::NONE,
            is_module_body: true,
            provided: Vec::new(),
        });
        Ok(Flow::Next)
    }

    fn resolve_module(&self, name: &str, from: &Path) -> Option<PathBuf> {
        let filename = format!("{name}.hy");
        let mut dirs: Vec<PathBuf> = Vec::new();
        // Same directory first, then the path list (§7).
        dirs.push(from.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(".")));
        dirs.extend(self.options.search_path.iter().cloned());
        for dir in dirs {
            let candidate = dir.join(&filename);
            if candidate.is_file() {
                return Some(candidate.canonicalize().unwrap_or(candidate));
            }
        }
        None
    }

    /// Bind a module's non-private names into the importer, and register the
    /// alias `::` selects through (§7).
    /// Bind a builtin module into the importer, under the same three rules a
    /// file gets: the qualifier alone, the names as well, or the qualifier
    /// under another name (§7).
    fn bind_builtin(&self, task: &Task, builtin: &'static str, alias: &str, unqualified: bool) {
        let importer = self.module(task.frame().module);
        importer
            .builtin_aliases
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(alias.to_string(), builtin);
        if unqualified {
            importer
                .builtin_aliases
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(builtin.to_string(), builtin);
            let mut star = importer.star_builtins.write().unwrap_or_else(|e| e.into_inner());
            star.retain(|m| *m != builtin);
            // Most recent `use` wins, so the newest goes first (§7).
            star.insert(0, builtin);
        }
    }

    /// Bind a module into the importer (§7).
    ///
    /// `use fs` registers the qualifier and nothing else, so `fs::read` reaches
    /// it and a bare `read` does not. `as *` binds the names unqualified as
    /// well — the qualifier stays, because otherwise two star-imports that
    /// collide would have no way to say which one is meant. `as filesystem`
    /// registers that qualifier *instead* of the module's own name.
    fn bind_module(
        &self,
        task: &Task,
        module: usize,
        name: &str,
        alias: &str,
        unqualified: bool,
    ) {
        let importer = task.frame().module;
        if unqualified {
            let exported_scope = self.module_scope(module);
            let exported: Vec<(String, Cell)> = exported_scope
                .names()
                .into_iter()
                .filter(|n| !is_private(n))
                .filter_map(|n| exported_scope.get_local(&n).map(|c| (n.to_string(), c)))
                .collect();
            let importer_module = self.module(importer);
            let mut imports = importer_module.imports.write().unwrap_or_else(|e| e.into_inner());
            for (name, cell) in exported {
                // Most recent `use` wins, so binding unconditionally is what
                // makes unqualified lookup match source order (§7). Earlier
                // ones stay behind it as call candidates.
                let slot = imports.entry(name).or_default();
                slot.retain(|existing| !Arc::ptr_eq(existing, &cell));
                slot.insert(0, cell);
            }
        }
        let importer_module = self.module(importer);
        let mut aliases = importer_module.aliases.write().unwrap_or_else(|e| e.into_inner());
        aliases.insert(alias.to_string(), module);
        if unqualified {
            aliases.insert(name.to_string(), module);
        }
    }
}

/// The arguments of one call, split the way the syntax splits them.
struct CallArgs {
    positional: Vec<Value>,
    named: Vec<(Arc<str>, Value)>,
}

impl CallArgs {
    fn describe(&self) -> String {
        let named: Vec<String> = self.named.iter().map(|(n, _)| format!("{n} =")).collect();
        if named.is_empty() {
            format!("{} argument(s)", self.positional.len())
        } else {
            format!("{} positional and {}", self.positional.len(), named.join(", "))
        }
    }
}

/// Pop one call's arguments: the positional ones, then the named ones in the
/// order they were written.
fn take_args(task: &mut Task, positional: usize, names: &[Arc<str>]) -> CallArgs {
    let at = task.stack.len() - names.len();
    let named_values: Vec<Value> = task.stack.split_off(at);
    let at = task.stack.len() - positional;
    let positional = task.stack.split_off(at);
    CallArgs { positional, named: names.iter().cloned().zip(named_values).collect() }
}

/// What one parameter expects, for a closure or a builtin alike.
struct ParamSpec {
    /// Empty for the bare `*`, which binds nothing (channels §6.1).
    name: Arc<str>,
    has_default: bool,
    by_ref: bool,
    /// A `*`: it collects what is left of the positional arguments, which is
    /// also what makes everything after it fillable by name only.
    variadic: bool,
}

fn param_specs(callee: &Value) -> Option<Vec<ParamSpec>> {
    match callee {
        Value::Fn(closure) => Some(
            closure
                .params
                .iter()
                .map(|p| ParamSpec {
                    name: p.name.clone(),
                    has_default: p.has_default,
                    by_ref: p.by_ref,
                    variadic: p.variadic,
                })
                .collect(),
        ),
        Value::Native(native) => Some(
            native
                .param_names()
                .iter()
                .enumerate()
                .map(|(i, name)| ParamSpec {
                    name: Arc::from(*name),
                    has_default: i >= native.required(),
                    by_ref: native.by_ref().get(i) == Some(&true),
                    variadic: native.variadic() == Some(i),
                })
                .collect(),
        ),
        _ => None,
    }
}

fn signature_of(callee: &Value) -> String {
    match callee {
        Value::Fn(closure) => closure.signature(),
        Value::Native(native) => native.signature().to_string(),
        other => other.kind().to_string(),
    }
}

/// Match a call's arguments to a candidate's parameters.
///
/// `None` means the candidate **rejects** the call — too many arguments, a name
/// it does not have, a parameter given twice, or one it needs and did not get.
/// That is what makes the next candidate worth trying (§3).
fn bind_args(callee: &Value, args: &CallArgs) -> Option<Vec<Option<Value>>> {
    let specs = param_specs(callee)?;
    let mut bound: Vec<Option<Value>> = vec![None; specs.len()];

    match specs.iter().position(|s| s.variadic) {
        // Positional filling stops at the `*`; what is left of the arguments
        // collects into it, and a bare `*` collects nothing, so anything left
        // over is a rejection (channels §6.1).
        Some(at) => {
            for (i, value) in args.positional.iter().take(at).enumerate() {
                bound[i] = Some(value.clone());
            }
            let rest: Vec<Value> = args.positional.iter().skip(at).cloned().collect();
            if specs[at].name.is_empty() {
                if !rest.is_empty() {
                    return None;
                }
            } else {
                bound[at] = Some(new_list(rest));
            }
        }
        None => {
            if args.positional.len() > specs.len() {
                return None;
            }
            for (i, value) in args.positional.iter().enumerate() {
                bound[i] = Some(value.clone());
            }
        }
    }

    for (name, value) in &args.named {
        let index = specs
            .iter()
            .position(|s| !s.name.is_empty() && s.name.as_ref() == name.as_ref())?;
        // A variadic collects positionally or not at all: naming it would put
        // one value where a list belongs.
        if specs[index].variadic || bound[index].is_some() {
            return None;
        }
        bound[index] = Some(value.clone());
    }

    for (spec, value) in specs.iter().zip(bound.iter()) {
        if value.is_none() && !spec.has_default && !spec.variadic {
            return None;
        }
    }
    Some(bound)
}

/// True where a candidate has a `*`, and so never rejects a call for having too
/// many arguments. Those are tried after every concrete arity (channels §6.1).
fn is_variadic(callee: &Value) -> bool {
    param_specs(callee).is_some_and(|specs| specs.iter().any(|s| s.variadic))
}

/// The crash for a call nothing accepted, listing what was tried (§8).
fn rejected(name: Option<&str>, candidates: &[Value], args: &CallArgs) -> Crash {
    let callable: Vec<String> =
        candidates.iter().filter(|c| param_specs(c).is_some()).map(signature_of).collect();
    if callable.is_empty() {
        let kind = candidates.first().map(|c| c.kind()).unwrap_or("value");
        return Crash::new(format!("cannot call a {kind}"));
    }
    let called = match name {
        Some(name) => format!("`{name}`"),
        None => "this function".to_string(),
    };
    Crash::new(format!(
        "no {called} accepts {}: tried {}",
        args.describe(),
        callable.join(", ")
    ))
}


#[derive(Clone, Copy, PartialEq, Eq)]
enum SendMode {
    Wait,
    Detach,
    Broadcast,
}

/// What a native answered: values to push, or a trail parked until a sibling
/// answers it (channels §6.5).
enum NativeOut {
    Values(Vec<Value>),
    Blocked,
}

/// `.null, .null` — no eligible sender is left (channels §1).
fn closed() -> Vec<Value> {
    vec![Value::null(), Value::null()]
}

fn index_of(channel: usize) -> Value {
    Value::Num(channel as f64)
}

fn send_mode(value: &Value) -> Result<SendMode, Crash> {
    match value {
        Value::Sym(s) if sym("wait") == *s => Ok(SendMode::Wait),
        Value::Sym(s) if sym("detach") == *s => Ok(SendMode::Detach),
        Value::Sym(s) if sym("broadcast") == *s => Ok(SendMode::Broadcast),
        other => Err(Crash::new(format!(
            "`send`'s mode is .wait, .detach or .broadcast, got {}",
            to_text(other)
        ))),
    }
}

/// The `to*` / `from*` list, which the variadic collected. Empty means "any".
fn channel_list(value: Option<Value>) -> Result<Option<Vec<usize>>, Crash> {
    let Some(value) = value else { return Ok(None) };
    let Value::List(list) = deref(&value)? else {
        return Ok(None);
    };
    let items = list.read().unwrap_or_else(|e| e.into_inner()).items.clone();
    if items.is_empty() {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match deref(&item)? {
            Value::Num(n) if n >= 0.0 && n.fract() == 0.0 => out.push(n as usize),
            other => {
                return Err(Crash::new(format!(
                    "a channel is a trail's index: a whole number from 0, got {}",
                    to_text(&other)
                )))
            }
        }
    }
    Ok(Some(out))
}

/// An index that cannot exist is a crash, and so is naming yourself: a trail's
/// siblings are the only ones it can reach (channels §3, §6.7).
fn check_indices(block: &BlockCtx, me: usize, only: Option<&Vec<usize>>) -> Result<(), Crash> {
    let Some(indices) = only else { return Ok(()) };
    // While the block is still spawning, an index past the last trail may yet
    // be one — unless the block's trail count was known before it ran.
    let known = block.arity.or(if block.spawn_done { Some(block.children.len()) } else { None });
    for index in indices {
        if *index == me {
            return Err(Crash::new(format!(
                "trail {me} is this one: a trail sends to its siblings, not to itself"
            )));
        }
        if let Some(count) = known {
            if *index >= count {
                return Err(Crash::new(format!(
                    "no trail {index}: this block has {count} trail{}, 0 to {}",
                    if count == 1 { "" } else { "s" },
                    count.saturating_sub(1)
                )));
            }
        }
    }
    Ok(())
}

/// Take the trail parked in `receive` at `target`, if it will have `from`.
fn take_waiter(block: &mut BlockCtx, target: usize, from: usize) -> Option<TaskId> {
    let at = block.waiting.iter().position(|w| {
        w.channel == target && w.from.as_ref().is_none_or(|only| only.contains(&from))
    })?;
    Some(block.waiting.remove(at).task)
}

/// The first parked receiver among `targets` — "first to receive wins" is
/// decided here, in park order (channels §4).
fn take_any_waiter(block: &mut BlockCtx, targets: &[usize], from: usize) -> Option<TaskId> {
    let at = block.waiting.iter().position(|w| {
        targets.contains(&w.channel)
            && w.from.as_ref().is_none_or(|only| only.contains(&from))
    })?;
    Some(block.waiting.remove(at).task)
}

/// A block whose every live trail is parked, with nothing buffered that anyone
/// will take, can never make progress. That is a crash, not a hang
/// (channels §6.6).
fn park_check(block: &BlockCtx) -> Result<(), Crash> {
    let parked = block.waiting.len() + block.mail.iter().filter(|m| m.waiter.is_some()).count();
    if block.spawn_done && block.pending > 0 && parked >= block.pending {
        return Err(Crash::new(format!(
            "every trail in this block is waiting: {parked} of them, and nothing left to send"
        )));
    }
    Ok(())
}

/// The waiters a change has just made unanswerable: no eligible peer is left,
/// so a parked `receive` gets `.null, .null` and a parked `send` gets `.false`
/// (channels §1, §5).
fn closed_waiters(block: &mut BlockCtx) -> Vec<(TaskId, Vec<Value>)> {
    let mut woken = Vec::new();
    let mut kept = Vec::with_capacity(block.waiting.len());
    for waiter in std::mem::take(&mut block.waiting) {
        if block.any_peer_left(waiter.channel, waiter.from.as_ref()) {
            kept.push(waiter);
        } else {
            woken.push((waiter.task, closed()));
        }
    }
    block.waiting = kept;

    let mut left = VecDeque::with_capacity(block.mail.len());
    for msg in std::mem::take(&mut block.mail) {
        let reachable = match &msg.to {
            Some(indices) => indices.iter().any(|i| !block.is_finished(*i)),
            None => (0..block.children.len()).any(|i| i != msg.from && !block.is_finished(i)),
        };
        match (reachable, msg.waiter) {
            (false, Some(sender)) => woken.push((sender, vec![boolean(false)])),
            (false, None) => {}
            (true, _) => left.push_back(msg),
        }
    }
    block.mail = left;
    woken
}

/// Take a cancelled trail out of whatever it was parked on, with the answer a
/// closed channel gives.
fn cancel_waiter(block: &mut BlockCtx, id: TaskId) -> Vec<(TaskId, Vec<Value>)> {
    let mut woken = Vec::new();
    if let Some(at) = block.waiting.iter().position(|w| w.task == id) {
        block.waiting.remove(at);
        woken.push((id, closed()));
    }
    if let Some(at) = block.mail.iter().position(|m| m.waiter == Some(id)) {
        // Its offer goes with it: nobody may take a dead trail's value.
        block.mail.remove(at);
        woken.push((id, vec![boolean(false)]));
    }
    woken
}

/// Hand a parked trail its answer and make it ready. The task may be running on
/// another worker and so out of the table, which the wake token covers — the
/// same race the block join has.
fn deliver_all(sched: &mut Sched, deliveries: Vec<(TaskId, Vec<Value>)>) {
    for (id, values) in deliveries {
        sched.deliveries.insert(id, values);
        match sched.tasks.get_mut(&id) {
            Some(task) if task.state == TaskState::Blocked => {
                task.state = TaskState::Ready;
                sched.ready.push_back(id);
            }
            _ => {
                sched.wakes.insert(id);
            }
        }
    }
}

fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "main".into())
}

/// Run a source string, the way the CLI and the tests both want it.
pub fn run_source(src: &str, file: &str, options: Options) -> Result<RunResult, HydraError> {
    let vm = Arc::new(Vm::new(options));
    vm.run_source(src, file)
}

/// Run a file, resolving `use` relative to it.
pub fn run_file(path: &Path, options: Options) -> Result<RunResult, HydraError> {
    let src = std::fs::read_to_string(path).map_err(|e| {
        HydraError::new(format!("cannot read {}: {e}", path.display()), &path.display().to_string(), Pos::NONE)
    })?;
    let vm = Arc::new(Vm::new(options));
    let file = path.display().to_string();
    let program = parse(&src, &file)?;
    let chunk = compile_program(&program)?;
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let module = vm.new_module(&stem(path), canonical);
    vm.spawn_root(chunk, module);
    Ok(vm.run())
}
