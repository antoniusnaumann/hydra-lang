//! The interpreter (spec §5–§10).
//!
//! One thread, one run queue, one task per trail. Instructions are stepped a
//! budget at a time; a task suspends by simply not being stepped again, which
//! is what makes a trail's suspension free at any depth.
//!
//! The two rules that shape everything here:
//!
//! * **Store after check** (§9.5). `Declare` and `Store` ask whether the trail
//!   is still live *after* evaluating and *before* writing, so a cancelled
//!   trail's pending assignment simply does not happen.
//! * **A call is never interrupted** (§9.5). Cancellation is only noticed at a
//!   statement boundary of the trail's *own* body, so an in-flight call — and
//!   everything it invokes — runs to the end and no frame is abandoned.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::ast::BlockKind;
use crate::compile::{compile_program, Chunk, Instr, Root};
use crate::errors::{Crash, HydraError, Pos, Site};
use crate::lexer::is_private;
use crate::parser::parse;
use crate::scope::{Scope, ScopeRef};
use crate::sched::{
    BlockCtx, CancelFlag, Frame, IterState, OnReturn, RunQueue, ScopeSlot, Task, TaskId, TaskState,
};
use crate::value::{
    binary_op, boolean, copy_value, deref, get_member, new_dict, new_list, path_segment, read_place,
    sym, to_text, unary_op, write_place, Cell, Closure, Native, PathSeg, RefValue, Sym, Value,
};

#[derive(Clone, Debug)]
pub struct Options {
    /// Dead-trail crashes are fatal under strict mode, so tests fail on bugs
    /// that production would swallow (§9.5).
    pub strict: bool,
    /// Dead-trail crashes are reported on stderr by default; silenceable.
    pub report_dead_crashes: bool,
    /// How many statement boundaries a trail runs before the scheduler looks
    /// at the others. §9.1 asks for a preemption check at every statement
    /// boundary, so 1 is the finest — and the default: it makes a trail's
    /// progress independent of how long its siblings are, and keeps a runaway
    /// loop from starving the block it is in. A larger value trades
    /// interleaving for scheduler overhead.
    pub step_budget: u32,
    pub search_path: Vec<PathBuf>,
}

impl Default for Options {
    fn default() -> Options {
        let search_path = std::env::var("HYDRA_PATH")
            .map(|v| v.split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect())
            .unwrap_or_default();
        Options { strict: false, report_dead_crashes: true, step_budget: 1, search_path }
    }
}

pub struct ModuleRt {
    pub name: String,
    pub path: PathBuf,
    pub scope: ScopeRef,
    /// Names `use` brought in, consulted after the lexical chain (§7).
    pub imports: RefCell<HashMap<String, Cell>>,
    /// `alias -> module`, for `mod::name`.
    pub aliases: RefCell<HashMap<String, usize>>,
}

enum Flow {
    Next,
    Yield,
    Blocked,
    /// The trail is cancelled and stops here (§9.5).
    Stop,
    Done,
}

pub struct Vm {
    pub options: Options,
    modules: Vec<ModuleRt>,
    module_by_path: HashMap<PathBuf, usize>,
    tasks: HashMap<TaskId, Task>,
    ready: RunQueue,
    next_id: TaskId,
    root_cancel: Rc<CancelFlag>,
    /// The first crash in a live trail: it ends the program (§8).
    pub crash: Option<Crash>,
    /// Crashes isolated to a dead trail (§9.5).
    pub dead_crashes: Vec<Crash>,
}

#[derive(Debug)]
pub struct RunResult {
    pub crash: Option<Crash>,
    pub dead_crashes: Vec<Crash>,
    pub root_scope: ScopeRef,
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
            modules: Vec::new(),
            module_by_path: HashMap::new(),
            tasks: HashMap::new(),
            ready: RunQueue::default(),
            next_id: 0,
            root_cancel: CancelFlag::root(),
            crash: None,
            dead_crashes: Vec::new(),
        }
    }

    fn new_module(&mut self, name: &str, path: PathBuf) -> usize {
        let id = self.modules.len();
        self.modules.push(ModuleRt {
            name: name.to_string(),
            path: path.clone(),
            scope: Scope::root(),
            imports: RefCell::new(HashMap::new()),
            aliases: RefCell::new(HashMap::new()),
        });
        if !path.as_os_str().is_empty() {
            self.module_by_path.insert(path, id);
        }
        id
    }

    /// Compile and run a source string as the main module.
    pub fn run_source(&mut self, src: &str, file: &str) -> Result<RunResult, HydraError> {
        let program = parse(src, file)?;
        let chunk = compile_program(&program)?;
        let path = PathBuf::from(file);
        let module = self.new_module(&stem(&path), path);
        self.spawn_root(chunk, module);
        Ok(self.run())
    }

    pub fn module_scope(&self, id: usize) -> ScopeRef {
        self.modules[id].scope.clone()
    }

    fn spawn_root(&mut self, chunk: Rc<Chunk>, module: usize) {
        let scope = self.modules[module].scope.clone();
        let id = self.next_id;
        self.next_id += 1;
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
            }],
            stack: Vec::new(),
            cancel: self.root_cancel.clone(),
            parent: None,
            blocks: Vec::new(),
            state: TaskState::Ready,
            base_depth: 1,
            is_trail: false,
            module,
        };
        self.tasks.insert(id, task);
        self.ready.push(id);
    }

    /// Run until nothing is left to run.
    ///
    /// Orphaned losers of a `race` are still in the queue, so the program waits
    /// for them at exit (§9.4's recommendation).
    pub fn run(&mut self) -> RunResult {
        while let Some(id) = self.ready.pop() {
            let Some(mut task) = self.tasks.remove(&id) else { continue };
            if task.state == TaskState::Blocked {
                self.tasks.insert(id, task);
                continue;
            }
            match self.run_slice(&mut task) {
                Ok(Flow::Yield) => {
                    self.tasks.insert(id, task);
                    self.ready.push(id);
                }
                Ok(Flow::Blocked) => {
                    task.state = TaskState::Blocked;
                    self.tasks.insert(id, task);
                }
                Ok(Flow::Done) | Ok(Flow::Stop) => self.finish_task(task, Ok(())),
                Ok(Flow::Next) => unreachable!("a slice never ends mid-step"),
                Err(crash) => self.finish_task(task, Err(crash)),
            }
        }
        RunResult {
            crash: self.crash.clone(),
            dead_crashes: self.dead_crashes.clone(),
            root_scope: self.modules[0].scope.clone(),
        }
    }

    fn run_slice(&mut self, task: &mut Task) -> Result<Flow, Crash> {
        let mut budget = self.options.step_budget;
        loop {
            if task.frames.is_empty() {
                return Ok(Flow::Done);
            }
            match self.step(task, &mut budget) {
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

    fn step(&mut self, task: &mut Task, budget: &mut u32) -> Result<Flow, Crash> {
        let instr = {
            let frame = task.frame_mut();
            if frame.ip >= frame.chunk.code.len() {
                // Falling off the end of a body returns nothing.
                Instr::ReturnNull
            } else {
                let instr = frame.chunk.code[frame.ip].clone();
                frame.ip += 1;
                instr
            }
        };

        match instr {
            Instr::PushNum(n) => task.push(Value::Num(n)),
            Instr::PushStr(s) => task.push(Value::Str(s)),
            Instr::PushSym(s) => task.push(Value::Sym(s)),
            Instr::Pop => {
                task.pop();
            }
            Instr::MakeList(n) => {
                let at = task.stack.len() - n;
                let items: Vec<Value> = task.stack.split_off(at);
                task.push(new_list(items));
            }
            Instr::MakeDict(n) => {
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
                let at = task.stack.len() - n;
                let parts: Vec<Value> = task.stack.split_off(at);
                let mut text = String::new();
                for part in &parts {
                    text.push_str(&to_text(part));
                }
                task.push(Value::Str(Rc::from(text.as_str())));
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
                task.push(Value::Fn(Rc::new(Closure {
                    name: name.to_string(),
                    params: params.as_ref().clone(),
                    chunk,
                    scope,
                })));
            }

            Instr::Load(root) => {
                let value = self.load(task, &root)?;
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
                if task.scope().get_local(&name).is_some() {
                    let child = Scope::child(task.scope());
                    task.frame_mut().push_scope(child, true);
                }
                task.scope().declare(&name, value);
            }
            Instr::Store { root, segs } => {
                let value = task.pop();
                let path = self.take_path(task, segs)?;
                // Evaluate → check the cancel flag → only then store (§9.5).
                if task.should_stop() {
                    return Ok(Flow::Stop);
                }
                let cell = self.cell_for(task, &root)?;
                write_place(&cell, &path, value)?;
            }
            Instr::MakeRef { root, segs } => {
                let path = self.take_path(task, segs)?;
                let cell = self.cell_for(task, &root)?;
                task.push(Value::Ref(RefValue { root: cell, path: Rc::new(path) }));
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
                    task.frame_mut().ip = target;
                }
            }
            Instr::AndJump(target) => {
                let keep = !task.stack.last().expect("operand").truthy();
                if keep {
                    task.frame_mut().ip = target;
                } else {
                    task.pop();
                }
            }
            Instr::OrJump(target) => {
                let keep = task.stack.last().expect("operand").truthy();
                if keep {
                    task.frame_mut().ip = target;
                } else {
                    task.pop();
                }
            }

            Instr::Call(argc) => {
                let at = task.stack.len() - argc;
                let args: Vec<Value> = task.stack.split_off(at);
                let callee = task.pop();
                return self.call(task, callee, args);
            }
            Instr::Return => {
                let value = task.pop();
                return Ok(self.pop_frame(task, value));
            }
            Instr::ReturnNull => {
                return Ok(self.pop_frame(task, Value::null()));
            }

            Instr::PushScope => {
                let child = Scope::child(task.scope());
                task.frame_mut().push_scope(child, false);
            }
            Instr::PopScope(n) => task.frame_mut().pop_scopes(n),

            Instr::IterStart => {
                let value = deref(&task.pop())?;
                match value {
                    Value::List(list) => {
                        list.borrow().shared.set(true);
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
                    let data = iter.list.borrow();
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
                    None => task.frame_mut().ip = exit,
                }
            }
            Instr::IterDrop => {
                task.frame_mut().iters.pop();
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

            Instr::BeginBlock { kind, .. } => {
                task.blocks.push(BlockCtx { kind, children: Vec::new(), pending: 0, decided: false });
            }
            Instr::SpawnTrail { body, var, column } => {
                let init = var.map(|name| (name, task.pop()));
                self.spawn_trail(task, body, init, column);
            }
            Instr::JoinBlock => {
                let satisfied = match task.blocks.last() {
                    Some(block) => match block.kind {
                        // Control passes `end` only when all have finished (§9.3).
                        BlockKind::Parallel => block.pending == 0,
                        // Decided by the first completion; losers are cancelled
                        // and `end` releases control at once (§9.4).
                        BlockKind::Race => block.decided || block.pending == 0,
                    },
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
            Instr::Use(name) => return self.use_module(task, &name),
        }
        Ok(Flow::Next)
    }

    // --- calls and frames ---------------------------------------------------

    fn call(&mut self, task: &mut Task, callee: Value, args: Vec<Value>) -> Result<Flow, Crash> {
        match callee {
            Value::Fn(closure) => {
                if args.len() != closure.params.len() {
                    return Err(Crash::new(format!(
                        "`{}` takes {} argument(s), got {}",
                        if closure.name.is_empty() { "fn" } else { &closure.name },
                        closure.params.len(),
                        args.len()
                    )));
                }
                let scope = Scope::child(&closure.scope);
                for (param, arg) in closure.params.iter().zip(args) {
                    scope.declare(param, arg);
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
                });
                Ok(Flow::Next)
            }
            Value::Native(native) => {
                if args.len() != native.arity() {
                    return Err(Crash::new(format!(
                        "`{}` takes {} argument(s), got {}",
                        native.name(),
                        native.arity(),
                        args.len()
                    )));
                }
                match native {
                    // Dynamic, no token threading, `.true` outside any trail —
                    // and false during crash shutdown too (§9.5).
                    Native::Alive => {
                        let alive = !task.cancel.is_cancelled();
                        task.push(boolean(alive));
                    }
                }
                Ok(Flow::Next)
            }
            other => Err(Crash::new(format!("cannot call a {}", other.kind()))),
        }
    }

    fn pop_frame(&mut self, task: &mut Task, value: Value) -> Flow {
        let frame = task.frames.pop().expect("a frame to return from");
        task.stack.truncate(frame.stack_base);
        if frame.is_module_body {
            // Declarations may have opened shadowing levels, so the namespace
            // a module exports is the scope it *ended* with (§6, §7).
            self.modules[frame.module].scope = frame.scope().clone();
        }
        match frame.on_return {
            OnReturn::PushValue => {
                if task.frames.is_empty() {
                    return Flow::Done;
                }
                task.push(value);
            }
            OnReturn::BindModule { alias, module } => {
                self.bind_module(task, module, &alias);
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
        let found = self.modules[module].imports.borrow().get(name).cloned();
        found
    }

    fn lookup_ns(&self, task: &Task, module: &str, name: &str) -> Result<Cell, Crash> {
        let importer = task.frame().module;
        let target = self.modules[importer].aliases.borrow().get(module).copied();
        let Some(target) = target else {
            return Err(Crash::new(format!("no module `{module}` is in scope; `use {module}` first")));
        };
        // Private names are not reachable through `::` (§7).
        if is_private(name) {
            return Err(Crash::new(format!(
                "`{name}` is private to module `{module}` and cannot be selected"
            )));
        }
        self.modules[target]
            .scope
            .get_local(name)
            .ok_or_else(|| Crash::new(format!("module `{module}` has no name `{name}`")))
    }

    fn load(&self, task: &Task, root: &Root) -> Result<Value, Crash> {
        let cell = match root {
            Root::Name(name) => match self.lookup(task, name) {
                Some(cell) => cell,
                // `alive()` is the only primitive (§9.5); a program may shadow
                // it, which is why the scope chain is consulted first.
                None if &**name == "alive" => return Ok(Value::Native(Native::Alive)),
                None => {
                    return Err(Crash::new(format!("`{name}` is not declared")));
                }
            },
            Root::Ns { module, name } => self.lookup_ns(task, module, name)?,
        };
        let value = cell.borrow().clone();
        match value {
            Value::Ref(r) => read_place(&r.root, &r.path),
            other => Ok(copy_value(&other)),
        }
    }

    // --- trails -------------------------------------------------------------

    fn spawn_trail(
        &mut self,
        task: &mut Task,
        body: Rc<Chunk>,
        init: Option<(Rc<str>, Value)>,
        _column: usize,
    ) {
        // A trail's scope's parent is the block's enclosing scope, so the trail
        // reads and writes the parent's bindings while everything it declares
        // with `:=` stays local to it (§6).
        let scope = Scope::child(task.scope());
        if let Some((name, value)) = init {
            scope.declare(&name, value);
        }
        let id = self.next_id;
        self.next_id += 1;
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
            }],
            stack: Vec::new(),
            cancel: CancelFlag::child(&task.cancel),
            parent: Some(task.id),
            blocks: Vec::new(),
            state: TaskState::Ready,
            base_depth: 1,
            is_trail: true,
            module: task.module,
        };
        if let Some(block) = task.blocks.last_mut() {
            block.children.push(id);
            block.pending += 1;
        }
        self.tasks.insert(id, child);
        self.ready.push(id);
    }

    fn finish_task(&mut self, task: Task, result: Result<(), Crash>) {
        if let Err(crash) = result {
            if task.cancel.is_cancelled() {
                // A crash inside a dead trail is isolated: that trail ends, the
                // program continues (§9.5).
                if self.options.report_dead_crashes {
                    eprintln!("hydra: crash in a cancelled trail (isolated): {crash}");
                }
                self.dead_crashes.push(crash.clone());
                if self.options.strict {
                    self.fatal(crash);
                }
            } else {
                self.fatal(crash);
            }
        }

        let Some(parent_id) = task.parent else { return };
        let mut to_cancel: Vec<TaskId> = Vec::new();
        let mut wake = false;
        if let Some(parent) = self.tasks.get_mut(&parent_id) {
            if let Some(block) = parent.blocks.iter_mut().find(|b| b.children.contains(&task.id)) {
                block.pending -= 1;
                match block.kind {
                    BlockKind::Parallel => wake = block.pending == 0,
                    BlockKind::Race => {
                        if !block.decided {
                            block.decided = true;
                            wake = true;
                            to_cancel.extend(block.children.iter().filter(|c| **c != task.id));
                        } else {
                            wake = block.pending == 0;
                        }
                    }
                }
            }
            if wake && parent.state == TaskState::Blocked {
                parent.state = TaskState::Ready;
            } else {
                wake = false;
            }
        }
        // Losers are cancelled (§9.4); they are never interrupted, so they stop
        // at their own next statement boundary.
        for id in to_cancel {
            if let Some(sibling) = self.tasks.get(&id) {
                sibling.cancel.cancel();
            }
        }
        if wake {
            self.ready.push(parent_id);
        }
    }

    /// A crash in a live trail: mark every sibling cancelled, keep the first
    /// diagnostic, and let the program drain (§8).
    fn fatal(&mut self, crash: Crash) {
        if self.crash.is_none() {
            self.crash = Some(crash);
        }
        self.root_cancel.cancel();
    }

    // --- modules (§7) -------------------------------------------------------

    fn use_module(&mut self, task: &mut Task, name: &str) -> Result<Flow, Crash> {
        let importer = task.frame().module;
        let from = self.modules[importer].path.clone();
        let Some(path) = self.resolve_module(name, &from) else {
            return Err(Crash::new(format!(
                "cannot find module `{name}`: no `{name}.hy` beside {} or on HYDRA_PATH",
                from.display()
            )));
        };

        if let Some(&id) = self.module_by_path.get(&path) {
            // Executing is skipped if the file is already in scope, but binding
            // always runs — including for a circular import, which resolves to
            // whatever is bound so far (§7).
            self.bind_module(task, id, name);
            return Ok(Flow::Next);
        }

        let src = std::fs::read_to_string(&path)
            .map_err(|e| Crash::new(format!("cannot read {}: {e}", path.display())))?;
        let file = path.display().to_string();
        let program = parse(&src, &file).map_err(|e| Crash::new(e.to_string()))?;
        let chunk = compile_program(&program).map_err(|e| Crash::new(e.to_string()))?;
        let id = self.new_module(name, path);
        let scope = self.modules[id].scope.clone();
        let stack_base = task.stack.len();
        task.frames.push(Frame {
            chunk,
            ip: 0,
            scopes: vec![ScopeSlot { scope, dynamic: false }],
            iters: Vec::new(),
            stack_base,
            module: id,
            on_return: OnReturn::BindModule { alias: Rc::from(name), module: id },
            call_site: Pos::NONE,
            is_module_body: true,
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
    fn bind_module(&mut self, task: &Task, module: usize, alias: &str) {
        let importer = task.frame().module;
        let exported: Vec<(String, Cell)> = self.modules[module]
            .scope
            .names()
            .into_iter()
            .filter(|n| !is_private(n))
            .filter_map(|n| self.modules[module].scope.get_local(&n).map(|c| (n.to_string(), c)))
            .collect();
        {
            let mut imports = self.modules[importer].imports.borrow_mut();
            for (name, cell) in exported {
                // Most recent `use` wins, so binding unconditionally is what
                // makes unqualified lookup match source order (§7).
                imports.insert(name, cell);
            }
        }
        self.modules[importer].aliases.borrow_mut().insert(alias.to_string(), module);
    }
}

fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "main".into())
}

/// Run a source string, the way the CLI and the tests both want it.
pub fn run_source(src: &str, file: &str, options: Options) -> Result<RunResult, HydraError> {
    let mut vm = Vm::new(options);
    vm.run_source(src, file)
}

/// Run a file, resolving `use` relative to it.
pub fn run_file(path: &Path, options: Options) -> Result<RunResult, HydraError> {
    let src = std::fs::read_to_string(path).map_err(|e| {
        HydraError::new(format!("cannot read {}: {e}", path.display()), &path.display().to_string(), Pos::NONE)
    })?;
    let mut vm = Vm::new(options);
    let file = path.display().to_string();
    let program = parse(&src, &file)?;
    let chunk = compile_program(&program)?;
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let module = vm.new_module(&stem(path), canonical);
    vm.spawn_root(chunk, module);
    Ok(vm.run())
}
