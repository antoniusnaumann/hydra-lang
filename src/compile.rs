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
//! pointers (§6), and names are still resolved dynamically.

use std::rc::Rc;

use crate::ast::*;
use crate::errors::{HydraError, Pos, Result};
use crate::value::{sym, Sym};

/// Where a place expression is rooted. Only a variable — possibly selected out
/// of a module — can root an assignment or a `&` (§5.1).
#[derive(Clone, Debug)]
pub enum Root {
    Name(Rc<str>),
    Ns { module: Rc<str>, name: Rc<str> },
}

impl Root {
    pub fn describe(&self) -> String {
        match self {
            Root::Name(n) => n.to_string(),
            Root::Ns { module, name } => format!("{module}::{name}"),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Instr {
    PushNum(f64),
    PushStr(Rc<str>),
    PushSym(Sym),
    MakeList(usize),
    /// Pops `n` key/value pairs. Keys are values rather than a static table
    /// because a key may be built at run time: `{ ."\(prefix)-id" : 1 }`.
    MakeDict(usize),
    /// Pops `n` values and concatenates their text forms: one `"…\(x)…"` (§1).
    Interpolate(usize),
    /// Pops a string and interns it as a symbol — the runtime half of a
    /// symbol built by interpolation (§2).
    MakeSym,
    MakeClosure { name: Rc<str>, params: Rc<Vec<ParamInfo>>, chunk: Rc<Chunk> },
    Pop,

    /// Read a variable: dereference a `&` transparently, then copy (§5.1).
    Load(Root),
    /// `x := v` — a fresh binding in the current scope, shadowing (§6).
    Declare(Rc<str>),
    /// `place = v` — writes into the binding an outward search finds (§6),
    /// path-copying shared nodes and creating a missing final key (§5).
    Store { root: Root, segs: usize },
    /// `&place` (§5.1).
    MakeRef { root: Root, segs: usize },
    /// `a[k]`, which is also what `a.k` compiles to (§5).
    GetMember,

    Binary(&'static str),
    Unary(&'static str),

    Jump(usize),
    JumpIfFalse(usize),
    /// `and`: falsy short-circuits and keeps its value.
    AndJump(usize),
    /// `or`: truthy short-circuits and keeps its value.
    OrJump(usize),

    Call(usize),
    Return,
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

    /// Jump past a parameter's default when the call supplied that argument.
    /// Defaults are evaluated in the *function's* scope, so a later one can
    /// refer to an earlier parameter.
    SkipIfProvided { index: usize, target: usize },

    /// Open a `parallel` / `race` block.
    BeginBlock { kind: BlockKind, label: Option<Rc<str>> },
    /// Start one trail of the open block. With `var`, the top of the stack
    /// becomes that binding in the trail's scope (`parallel for`).
    SpawnTrail { body: Rc<Chunk>, var: Option<Rc<str>>, column: usize },
    /// Wait for the open block: all trails, or the first (§9.3, §9.4).
    JoinBlock,

    /// `break trail` (§9.6).
    EndTrail,
    Use(Rc<str>),
}

/// What a call has to supply for one parameter.
#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub name: Rc<str>,
    /// `&name`: the argument must be a reference (§5.1).
    pub by_ref: bool,
    pub has_default: bool,
}

/// A compiled body: a function, a module, a trail, or a closure.
pub struct Chunk {
    pub name: String,
    pub file: Rc<str>,
    pub code: Vec<Instr>,
    /// One position per instruction, for crash diagnostics.
    pub pos: Vec<Pos>,
    pub params: Vec<ParamInfo>,
}

impl Chunk {
    pub fn required(&self) -> usize {
        self.params.iter().filter(|p| !p.has_default).count()
    }
}

impl std::fmt::Debug for Chunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Chunk({}, {} instrs)", self.name, self.code.len())
    }
}

/// What a `break` or `continue` can target.
struct LoopCtx {
    label: Option<String>,
    /// A trail body: `break` with no label ends the trail rather than a loop
    /// (§9.6), and a labelled `parallel` block resolves here too.
    is_trail: bool,
    /// A `for` loop owns an iterator that a `break` has to drop as it leaves.
    owns_iter: bool,
    scope_depth: usize,
    iter_depth: usize,
    breaks: Vec<usize>,
    continues: Vec<usize>,
}

pub struct Compiler {
    file: Rc<str>,
    code: Vec<Instr>,
    pos: Vec<Pos>,
    loops: Vec<LoopCtx>,
    scope_depth: usize,
    iter_depth: usize,
    /// True while compiling a trail body, so `return` can be rejected (§9.6).
    in_trail: bool,
}

pub fn compile_program(program: &Program) -> Result<Rc<Chunk>> {
    let mut c = Compiler::new(&program.file, false);
    c.block(&program.body)?;
    c.emit(Instr::ReturnNull, Pos::NONE);
    Ok(Rc::new(c.finish(program.file.clone(), Vec::new())))
}

impl Compiler {
    fn new(file: &str, in_trail: bool) -> Compiler {
        Compiler {
            file: Rc::from(file),
            code: Vec::new(),
            pos: Vec::new(),
            loops: Vec::new(),
            scope_depth: 0,
            iter_depth: 0,
            in_trail,
        }
    }

    fn finish(self, name: String, params: Vec<ParamInfo>) -> Chunk {
        Chunk { name, file: self.file, code: self.code, pos: self.pos, params }
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
            | Instr::JumpIfFalse(t)
            | Instr::AndJump(t)
            | Instr::OrJump(t)
            | Instr::IterNext { exit: t, .. }
            | Instr::SkipIfProvided { target: t, .. } => *t = target,
            other => panic!("cannot patch {other:?}"),
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
        self.emit(Instr::PushScope, pos);
        self.scope_depth += 1;
        self.block(body)?;
        self.scope_depth -= 1;
        self.emit(Instr::PopScope(1), pos);
        Ok(())
    }

    fn stmt(&mut self, stmt: &Stmt) -> Result<()> {
        let pos = stmt.pos();
        self.emit(Instr::Tick, pos);
        match stmt {
            Stmt::Use { module, pos } => {
                self.emit(Instr::Use(Rc::from(module.as_str())), *pos);
            }
            Stmt::FnDecl { name, def, pos } => {
                self.closure(def, name, *pos)?;
                self.emit(Instr::Declare(Rc::from(name.as_str())), *pos);
            }
            Stmt::Decl { name, value, pos } => {
                self.expr(value)?;
                self.emit(Instr::Declare(Rc::from(name.as_str())), *pos);
            }
            Stmt::Assign { target, value, pos } => {
                let (root, segs) = self.place(target)?;
                self.expr(value)?;
                self.emit(Instr::Store { root, segs }, *pos);
            }
            Stmt::Expr { expr, .. } => {
                self.expr(expr)?;
                self.emit(Instr::Pop, pos);
            }
            Stmt::Return { value, pos } => {
                if self.in_trail {
                    // §9.6: `check` rejects this too, with a better message.
                    return self.err("`return` inside a trail is not allowed", *pos);
                }
                match value {
                    Some(v) => {
                        self.expr(v)?;
                        self.emit(Instr::Return, *pos);
                    }
                    None => {
                        self.emit(Instr::ReturnNull, *pos);
                    }
                }
            }
            Stmt::If { branches, pos, end_pos } => self.compile_if(branches, *pos, *end_pos)?,
            Stmt::While { cond, body, label, pos, .. } => {
                self.compile_while(cond, body, label.as_deref(), *pos)?
            }
            Stmt::For { var, iterable, body, label, pos, .. } => {
                self.compile_for(var, iterable, body, label.as_deref(), *pos)?
            }
            Stmt::Break { target, pos } => self.compile_break(target, *pos)?,
            Stmt::Continue { label, pos } => self.compile_continue(label.as_deref(), *pos)?,
            Stmt::Parallel { kind, trails, label, pos, .. } => {
                self.emit(
                    Instr::BeginBlock { kind: *kind, label: label.as_deref().map(Rc::from) },
                    *pos,
                );
                for trail in trails {
                    let chunk = self.trail_chunk(&trail.body, trail.column, label.as_deref())?;
                    self.emit(
                        Instr::SpawnTrail { body: chunk, var: None, column: trail.column },
                        trail.pos,
                    );
                }
                self.emit(Instr::JoinBlock, *pos);
            }
            Stmt::ParallelFor { kind, var, iterable, body, label, pos, .. } => {
                self.emit(
                    Instr::BeginBlock { kind: *kind, label: label.as_deref().map(Rc::from) },
                    *pos,
                );
                let chunk = self.trail_chunk(body, 0, label.as_deref())?;
                self.expr(iterable)?;
                self.emit(Instr::IterStart, *pos);
                self.iter_depth += 1;
                let top = self.here();
                let next = self.emit(Instr::IterNext { exit: 0 }, *pos);
                self.emit(
                    Instr::SpawnTrail { body: chunk, var: Some(Rc::from(var.as_str())), column: 0 },
                    *pos,
                );
                self.emit(Instr::Jump(top), *pos);
                let exit = self.here();
                self.patch(next, exit);
                self.emit(Instr::IterDrop, *pos);
                self.iter_depth -= 1;
                self.emit(Instr::JoinBlock, *pos);
            }
            Stmt::ParallelWhile { kind, cond, body, label, pos, .. } => {
                self.emit(
                    Instr::BeginBlock { kind: *kind, label: label.as_deref().map(Rc::from) },
                    *pos,
                );
                let chunk = self.trail_chunk(body, 0, label.as_deref())?;
                let top = self.here();
                self.expr(cond)?;
                let exit = self.emit(Instr::JumpIfFalse(0), *pos);
                self.emit(Instr::SpawnTrail { body: chunk, var: None, column: 0 }, *pos);
                self.emit(Instr::Jump(top), *pos);
                let after = self.here();
                self.patch(exit, after);
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
        label: Option<&str>,
        pos: Pos,
    ) -> Result<()> {
        let top = self.here();
        self.expr(cond)?;
        let exit = self.emit(Instr::JumpIfFalse(0), pos);

        self.loops.push(LoopCtx {
            label: label.map(str::to_string),
            is_trail: false,
            owns_iter: false,
            scope_depth: self.scope_depth,
            iter_depth: self.iter_depth,
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
        label: Option<&str>,
        pos: Pos,
    ) -> Result<()> {
        self.expr(iterable)?;
        self.emit(Instr::IterStart, pos);
        self.iter_depth += 1;

        let top = self.here();
        let next = self.emit(Instr::IterNext { exit: 0 }, pos);

        self.loops.push(LoopCtx {
            label: label.map(str::to_string),
            is_trail: false,
            owns_iter: true,
            scope_depth: self.scope_depth,
            iter_depth: self.iter_depth,
            breaks: Vec::new(),
            continues: Vec::new(),
        });

        // The loop variable is declared in the body's own scope, fresh each
        // iteration, so a closure made in the body captures that iteration's
        // binding (§6).
        self.emit(Instr::PushScope, pos);
        self.scope_depth += 1;
        self.emit(Instr::Declare(Rc::from(var)), pos);
        self.block(body)?;
        self.scope_depth -= 1;
        self.emit(Instr::PopScope(1), pos);
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

    fn compile_break(&mut self, target: &BreakTarget, pos: Pos) -> Result<()> {
        match target {
            BreakTarget::Trail => {
                if !self.in_trail {
                    return self.err("`break trail` is only meaningful inside a trail", pos);
                }
                self.emit(Instr::EndTrail, pos);
                Ok(())
            }
            BreakTarget::Innermost => match self.loops.iter().rposition(|c| !c.is_trail) {
                Some(index) => self.jump_out_of(index, pos, true),
                None if self.in_trail => {
                    // `break` ends the current trail early (§9.6).
                    self.emit(Instr::EndTrail, pos);
                    Ok(())
                }
                None => self.err("`break` outside a loop", pos),
            },
            BreakTarget::Label(label) => {
                match self.loops.iter().rposition(|c| c.label.as_deref() == Some(label.as_str())) {
                    Some(index) if self.loops[index].is_trail => {
                        // A label on a `parallel` block names the block; from
                        // inside a trail the only thing breaking it can mean is
                        // ending this trail (see QUESTIONS.md §13).
                        self.emit(Instr::EndTrail, pos);
                        Ok(())
                    }
                    Some(index) => self.jump_out_of(index, pos, true),
                    None => self.err(format!("no loop or block labelled `{label}` is in scope"), pos),
                }
            }
        }
    }

    fn compile_continue(&mut self, label: Option<&str>, pos: Pos) -> Result<()> {
        let index = match label {
            None => self.loops.iter().rposition(|c| !c.is_trail),
            Some(l) => self
                .loops
                .iter()
                .rposition(|c| c.label.as_deref() == Some(l) && !c.is_trail),
        };
        match index {
            Some(index) => self.jump_out_of(index, pos, false),
            None => match label {
                Some(l) => self.err(format!("no loop labelled `{l}` is in scope"), pos),
                None => self.err("`continue` outside a loop", pos),
            },
        }
    }

    /// Unwind scopes and iterators back to a loop, then jump.
    fn jump_out_of(&mut self, index: usize, pos: Pos, is_break: bool) -> Result<()> {
        let (scope_depth, iter_depth, owns_iter) = {
            let ctx = &self.loops[index];
            (ctx.scope_depth, ctx.iter_depth, ctx.owns_iter)
        };
        let scopes = self.scope_depth - scope_depth;
        if scopes > 0 {
            self.emit(Instr::PopScope(scopes), pos);
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
                self.emit(Instr::Load(Root::Name(Rc::from(name.as_str()))), *pos);
            }
            Expr::Namespace { module, name, pos } => {
                self.emit(
                    Instr::Load(Root::Ns {
                        module: Rc::from(module.as_str()),
                        name: Rc::from(name.as_str()),
                    }),
                    *pos,
                );
            }
            Expr::Key { obj, key, pos } => {
                self.expr(obj)?;
                self.symbol(key)?;
                self.emit(Instr::GetMember, *pos);
            }
            Expr::Index { obj, index, pos } => {
                self.expr(obj)?;
                self.expr(index)?;
                self.emit(Instr::GetMember, *pos);
            }
            Expr::Call { callee, args, pos } => {
                self.expr(callee)?;
                for arg in args {
                    self.expr(arg)?;
                }
                self.emit(Instr::Call(args.len()), *pos);
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
                    self.emit(Instr::Binary(op), *pos);
                }
            },
            Expr::Closure(def) => self.closure(def, "", def.pos)?,
        }
        Ok(())
    }

    /// A string literal: one instruction when it is literal, a concatenation
    /// of its pieces when it interpolates (§1).
    fn interpolated(&mut self, parts: &[StrPart], pos: Pos) -> Result<()> {
        if parts.is_empty() {
            self.emit(Instr::PushStr(Rc::from("")), pos);
            return Ok(());
        }
        if let [StrPart::Text(text)] = parts {
            self.emit(Instr::PushStr(Rc::from(text.as_str())), pos);
            return Ok(());
        }
        for part in parts {
            match part {
                StrPart::Text(text) => {
                    self.emit(Instr::PushStr(Rc::from(text.as_str())), pos);
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

    fn closure(&mut self, def: &Rc<ClosureDef>, name: &str, pos: Pos) -> Result<()> {
        let mut sub = Compiler::new(&self.file, self.in_trail);
        // The prologue fills in the parameters the call did not supply.
        for (index, param) in def.params.iter().enumerate() {
            let Some(default) = &param.default else { continue };
            let skip = sub.emit(Instr::SkipIfProvided { index, target: 0 }, param.pos);
            sub.expr(default)?;
            sub.emit(Instr::Declare(Rc::from(param.name.as_str())), param.pos);
            let after = sub.here();
            sub.patch(skip, after);
        }
        match &def.body {
            ClosureBody::Expr(expr) => {
                sub.expr(expr)?;
                sub.emit(Instr::Return, def.pos);
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
                name: Rc::from(p.name.as_str()),
                by_ref: p.by_ref,
                has_default: p.default.is_some(),
            })
            .collect();
        let display = if name.is_empty() { "fn".to_string() } else { name.to_string() };
        let chunk = Rc::new(sub.finish(display, params.clone()));
        self.emit(
            Instr::MakeClosure { name: Rc::from(name), params: Rc::new(params), chunk },
            pos,
        );
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
                    let root = Root::Name(Rc::from(name.as_str()));
                    return self.emit_segments(root, segs);
                }
                Expr::Namespace { module, name, .. } => {
                    let root = Root::Ns {
                        module: Rc::from(module.as_str()),
                        name: Rc::from(name.as_str()),
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
    fn trail_chunk(
        &mut self,
        body: &[Stmt],
        column: usize,
        label: Option<&str>,
    ) -> Result<Rc<Chunk>> {
        let mut sub = Compiler::new(&self.file, true);
        sub.loops.push(LoopCtx {
            label: label.map(str::to_string),
            is_trail: true,
            owns_iter: false,
            scope_depth: 0,
            iter_depth: 0,
            breaks: Vec::new(),
            continues: Vec::new(),
        });
        sub.block(body)?;
        sub.emit(Instr::ReturnNull, Pos::NONE);
        let ctx = sub.loops.pop().expect("trail context");
        // A trail's `break` ends the trail; those are `EndTrail` instructions
        // already, so nothing should be waiting to be patched.
        debug_assert!(ctx.breaks.is_empty() && ctx.continues.is_empty());
        Ok(Rc::new(sub.finish(format!("trail {column}"), Vec::new())))
    }
}
