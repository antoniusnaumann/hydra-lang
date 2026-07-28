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
use std::io::Write;
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
    binary_op, boolean, copy_value, deref, get_member, member_opt, new_dict, new_list, path_segment,
    push_place, read_place, sym, to_text, unary_op, write_place, Cell, Closure, Native, PathSeg,
    RefValue, Sym, Value,
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
    ///
    /// A name can have more than one: the most recent `use` wins for an
    /// unqualified *read*, and a *call* may fall through to an earlier one that
    /// accepts it (§3).
    pub imports: RefCell<HashMap<String, Vec<Cell>>>,
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
                provided: Vec::new(),
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

            Instr::Call { positional, names } => {
                let args = take_args(task, positional, &names);
                let callee = task.pop();
                let Some(bound) = bind_args(&callee, &args) else {
                    return Err(rejected(None, &[callee], &args));
                };
                return self.enter(task, callee, bound);
            }
            Instr::CallName { name, positional, names } => {
                let args = take_args(task, positional, &names);
                // Every function bound to the name is a candidate, innermost
                // first; the first that accepts the call is the one (§3).
                let candidates = self.candidates(task, &name)?;
                if candidates.is_empty() {
                    return Err(Crash::new(format!("`{name}` is not declared")));
                }
                let chosen = candidates
                    .iter()
                    .find_map(|c| bind_args(c, &args).map(|bound| (c.clone(), bound)));
                let Some((callee, bound)) = chosen else {
                    return Err(rejected(Some(&name), &candidates, &args));
                };
                return self.enter(task, callee, bound);
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

            Instr::SkipIfProvided { index, target } => {
                if task.frame().provided.get(index) == Some(&true) {
                    task.frame_mut().ip = target;
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

    /// Every function the name could mean, innermost binding first, then the
    /// most recent `use` first, then the builtin (§3, §7).
    fn candidates(&self, task: &Task, name: &str) -> Result<Vec<Value>, Crash> {
        let mut cells: Vec<Cell> = task.scope().all_bindings(name);
        let module = task.frame().module;
        if let Some(imported) = self.modules[module].imports.borrow().get(name) {
            cells.extend(imported.iter().cloned());
        }
        let mut out = Vec::new();
        for cell in cells {
            let value = cell.borrow().clone();
            out.push(match value {
                Value::Ref(r) => read_place(&r.root, &r.path)?,
                other => copy_value(&other),
            });
        }
        if let Some(native) = Native::lookup(name) {
            out.push(Value::Native(native));
        }
        Ok(out)
    }

    /// Enter a call whose arguments are already matched to its parameters.
    fn enter(
        &mut self,
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
                        scope.declare(&param.name, value);
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
                let value = self.native(task, native, bound)?;
                task.push(value);
                Ok(Flow::Next)
            }
            other => Err(Crash::new(format!("cannot call a {}", other.kind()))),
        }
    }

    /// The builtins of `spec/hydra_stdlib.md`, plus `alive()` (§9.5).
    fn native(
        &mut self,
        task: &Task,
        native: Native,
        args: Vec<Option<Value>>,
    ) -> Result<Value, Crash> {
        let arg = |i: usize| args.get(i).cloned().flatten().unwrap_or_else(Value::null);
        match native {
            // Dynamic, no token threading, `.true` outside any trail — and
            // false during crash shutdown too (§9.5).
            Native::Alive => Ok(boolean(!task.cancel.is_cancelled())),
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
                Ok(Value::null())
            }
            Native::Has => Ok(boolean(member_opt(&arg(0), &arg(1))?.is_some())),
            Native::Get => Ok(member_opt(&arg(0), &arg(1))?.unwrap_or_else(|| arg(2))),
            Native::Len => {
                let value = deref(&arg(0))?;
                let len = match &value {
                    Value::List(rc) => rc.borrow().items.len(),
                    Value::Dict(rc) => rc.borrow().entries.len(),
                    // Characters, not bytes: source is UTF-8 (§1).
                    Value::Str(text) => text.chars().count(),
                    other => {
                        return Err(Crash::new(format!(
                            "`len` counts a list, a dict or a string, got a {}",
                            other.kind()
                        )))
                    }
                };
                Ok(Value::Num(len as f64))
            }
            Native::Push => {
                let Value::Ref(target) = arg(0) else {
                    unreachable!("checked by the by-reference rule above")
                };
                let len = push_place(&target.root, &target.path, arg(1))?;
                Ok(Value::Num(len as f64))
            }
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
        let found = self.modules[module].imports.borrow().get(name).and_then(|c| c.first().cloned());
        found
    }

    fn lookup_ns(&self, task: &Task, module: &str, name: &str) -> Result<Cell, Crash> {
        if module.is_empty() {
            return Err(Crash::new(format!("`::{name}` is a builtin, not a variable")));
        }
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
                provided: Vec::new(),
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
                // makes unqualified lookup match source order (§7). Earlier
                // ones stay behind it as call candidates.
                let slot = imports.entry(name).or_default();
                slot.retain(|existing| !Rc::ptr_eq(existing, &cell));
                slot.insert(0, cell);
            }
        }
        self.modules[importer].aliases.borrow_mut().insert(alias.to_string(), module);
    }
}

/// The arguments of one call, split the way the syntax splits them.
struct CallArgs {
    positional: Vec<Value>,
    named: Vec<(Rc<str>, Value)>,
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
fn take_args(task: &mut Task, positional: usize, names: &[Rc<str>]) -> CallArgs {
    let at = task.stack.len() - names.len();
    let named_values: Vec<Value> = task.stack.split_off(at);
    let at = task.stack.len() - positional;
    let positional = task.stack.split_off(at);
    CallArgs { positional, named: names.iter().cloned().zip(named_values).collect() }
}

/// What one parameter expects, for a closure or a builtin alike.
struct ParamSpec {
    name: Rc<str>,
    has_default: bool,
    by_ref: bool,
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
                })
                .collect(),
        ),
        Value::Native(native) => Some(
            native
                .param_names()
                .iter()
                .enumerate()
                .map(|(i, name)| ParamSpec {
                    name: Rc::from(*name),
                    has_default: i >= native.required(),
                    by_ref: native.by_ref().get(i) == Some(&true),
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
    if args.positional.len() > specs.len() {
        return None;
    }
    let mut bound: Vec<Option<Value>> = vec![None; specs.len()];
    for (i, value) in args.positional.iter().enumerate() {
        bound[i] = Some(value.clone());
    }
    for (name, value) in &args.named {
        let index = specs.iter().position(|s| s.name.as_ref() == name.as_ref())?;
        if bound[index].is_some() {
            return None;
        }
        bound[index] = Some(value.clone());
    }
    for (spec, value) in specs.iter().zip(bound.iter()) {
        if value.is_none() && !spec.has_default {
            return None;
        }
    }
    Some(bound)
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
