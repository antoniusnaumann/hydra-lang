//! `check` — static analysis (spec §11).
//!
//! **Principle: report only what is guaranteed to crash at runtime, never what
//! might.** This language has enough legitimate nondeterminism that a maybe-list
//! would be enormous and would be ignored within a week, so every rule here has
//! to be sure.
//!
//! Two things make that achievable. Value semantics (§5.1) mean passing a value
//! to a function cannot change it, so a dict's key set stays knowable unless
//! someone writes to the variable or takes a `&` of it. And `use` is
//! resolvable, so name resolution is exact — unless a module cannot be found,
//! in which case every name-resolution diagnostic is switched off rather than
//! guessed at.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::ast::*;
use crate::errors::{Diagnostic, Pos, Report, Site};
use crate::lexer::{is_private, Tok};
use crate::value::{Native, CHANNEL_NATIVES};
use crate::parser::parse;

#[derive(Clone, Debug, Default)]
pub struct CheckOptions {
    /// Names the host supplies. Without a standard library every call in the
    /// spec's own example is an undeclared name, so there has to be a way to
    /// say "these come from outside" (QUESTIONS.md §1).
    pub externs: Vec<String>,
    pub search_path: Vec<PathBuf>,
}

impl CheckOptions {
    pub fn from_env() -> CheckOptions {
        let search_path = std::env::var("HYDRA_PATH")
            .map(|v| v.split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect())
            .unwrap_or_default();
        CheckOptions { externs: Vec::new(), search_path }
    }
}

/// What a call to a known function has to supply.
#[derive(Clone, Debug, PartialEq)]
struct Signature {
    required: usize,
    total: usize,
    /// Which parameter is the `*`, if any: it collects the rest of the
    /// positional arguments, and everything after it is keyword-only.
    variadic: Option<usize>,
    /// The most values a call to it can answer with (channels §6.2).
    returns: usize,
    /// Which parameters the call must mark with `&` (§5.1).
    by_ref: Vec<bool>,
    /// Parameter names, so a named argument can be matched.
    names: Vec<String>,
    label: String,
}

impl Signature {
    fn of(def: &ClosureDef, name: &str) -> Signature {
        let params: Vec<String> = def
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
        Signature {
            required: def.required(),
            total: def.params.len(),
            variadic: def.params.iter().position(|p| p.variadic),
            returns: returns_of(def),
            by_ref: def.params.iter().map(|p| p.by_ref).collect(),
            names: def.params.iter().map(|p| p.name.clone()).collect(),
            label: format!("{name}({})", params.join(", ")),
        }
    }

    fn native(native: Native) -> Signature {
        Signature {
            required: native.required(),
            total: native.total(),
            variadic: native.variadic(),
            // `receive` answers with the value and the trail that sent it.
            returns: if native == Native::Receive { 2 } else { 1 },
            by_ref: native.by_ref().to_vec(),
            names: native.param_names().iter().map(|n| n.to_string()).collect(),
            label: native.signature().to_string(),
        }
    }

    /// Match a call's arguments to these parameters, or `None` if this
    /// signature rejects the call — the same rule the runtime applies (§3).
    fn bind<'a>(&self, args: &'a [Arg]) -> Option<Vec<Option<&'a Arg>>> {
        let mut bound: Vec<Option<&Arg>> = vec![None; self.total];
        let mut next = 0;
        for arg in args {
            match &arg.name {
                None => match self.variadic {
                    // Positional filling stops at the `*`, which takes as many
                    // as are left — a bare `*` takes none (channels §6.1).
                    Some(at) if next >= at => {
                        if self.names.get(at).is_some_and(|n| n.is_empty()) {
                            return None;
                        }
                        bound[at] = Some(arg);
                    }
                    _ => {
                        if next >= self.total {
                            return None;
                        }
                        bound[next] = Some(arg);
                        next += 1;
                    }
                },
                Some(name) => {
                    let index = self.names.iter().position(|n| n == name)?;
                    if bound[index].is_some() || self.variadic == Some(index) {
                        return None;
                    }
                    bound[index] = Some(arg);
                }
            }
        }
        for (index, slot) in bound.iter().enumerate() {
            if slot.is_none() && index < self.required && self.variadic != Some(index) {
                return None;
            }
        }
        Some(bound)
    }
}

/// The most values a call to this function can answer with. Falling off the
/// end answers with one — `.null` — so it is never fewer than that, and §11's
/// rule means only a name that is *guaranteed* to have nothing behind it is
/// reported (channels §6.2).
fn returns_of(def: &ClosureDef) -> usize {
    let body = match &def.body {
        ClosureBody::Expr(_) => return 1,
        ClosureBody::Block(body) => body,
    };
    let mut most = 1;
    walk_stmts(body, &mut |stmt| {
        if let Stmt::Return { values, .. } = stmt {
            most = most.max(values.len());
        }
    });
    most
}

#[derive(Clone, Debug, Default)]
struct Binding {
    pos: Pos,
    /// Set when the name is bound to a function whose signature is knowable.
    arity: Option<Signature>,
    /// Set when the name is bound to a dict literal and never written to, so
    /// its key set is exactly known.
    keys: Option<Vec<String>>,
}

/// A lexical scope while checking.
///
/// There is no need to distinguish a function's scope from a block's: a
/// closure captures the scope chain (§6), so a name visible outside is visible
/// inside, and lookup walks every enclosing scope either way.
#[derive(Default)]
struct CheckScope {
    names: HashMap<String, Binding>,
    /// Names a `parallel` block's trails declared, so a later read can be told
    /// what actually happened rather than just "not declared" (§6, §11).
    trail_locals: HashMap<String, Pos>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LabelKind {
    Loop,
    /// A trail body. `break` with no label ends it (§9.6).
    Trail,
}

struct Label {
    name: Option<String>,
    kind: LabelKind,
}

struct ModuleInfo {
    exports: HashMap<String, Option<Signature>>,
}

pub fn check_program(program: &Program, options: &CheckOptions) -> Report {
    let mut checker = Checker::new(&program.file, options);
    checker.run(program);
    checker.report
}

/// Parse and check a file, reporting a parse error as the one diagnostic.
pub fn check_file(path: &Path, options: &CheckOptions) -> Report {
    let file = path.display().to_string();
    let src = match std::fs::read_to_string(path) {
        Ok(src) => src,
        Err(e) => {
            let mut report = Report::default();
            report.error(format!("cannot read {file}: {e}"), Site::new(&file, Pos::NONE), "io");
            return report;
        }
    };
    match parse(&src, &file) {
        Ok(program) => check_program(&program, options),
        Err(e) => {
            let mut report = Report::default();
            report.diagnostics.push(Diagnostic::error(e.message, e.site, "syntax"));
            report
        }
    }
}

struct Checker<'a> {
    file: &'a str,
    report: Report,
    scopes: Vec<CheckScope>,
    labels: Vec<Label>,
    /// Lexical trail nesting. Reset inside a function body: a function does not
    /// know it is running in a trail.
    trail_depth: usize,
    race_depth: usize,
    /// While checking a trail of the row form: this trail's own index and how
    /// many the block has. That is the one shape whose trail count is known
    /// before the program runs, which is what lets a channel index that cannot
    /// exist be an error rather than a crash (channels §6.7).
    trail_shape: Option<(usize, usize)>,
    /// Names ever written to or `&`-referenced anywhere in the file. Coarse on
    /// purpose: it only ever *suppresses* diagnostics.
    mutated: HashSet<String>,
    /// Names declared more than once. A call one of them rejects goes to the
    /// next (§3), so no single signature is guaranteed.
    overloaded: HashSet<String>,
    read: HashSet<String>,
    externs: HashSet<String>,
    modules: HashMap<String, ModuleInfo>,
    /// Names `use` brought in: name -> (module alias, signature).
    imports: HashMap<String, (String, Option<Signature>)>,
    /// False as soon as one `use` cannot be resolved: then no name-resolution
    /// diagnostic is trustworthy, so none are emitted.
    names_are_knowable: bool,
    search_path: Vec<PathBuf>,
    dir: PathBuf,
}

impl<'a> Checker<'a> {
    fn new(file: &'a str, options: &CheckOptions) -> Checker<'a> {
        let dir = Path::new(file).parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        Checker {
            file,
            report: Report::default(),
            scopes: Vec::new(),
            labels: Vec::new(),
            trail_depth: 0,
            race_depth: 0,
            trail_shape: None,
            mutated: HashSet::new(),
            overloaded: HashSet::new(),
            read: HashSet::new(),
            externs: options.externs.iter().cloned().collect(),
            modules: HashMap::new(),
            imports: HashMap::new(),
            names_are_knowable: true,
            search_path: options.search_path.clone(),
            dir,
        }
    }

    fn site(&self, pos: Pos) -> Site {
        Site::new(self.file, pos)
    }

    fn error(&mut self, message: impl Into<String>, pos: Pos, code: &'static str) {
        let site = self.site(pos);
        self.report.error(message, site, code);
    }

    fn warn(&mut self, message: impl Into<String>, pos: Pos, code: &'static str) {
        let site = self.site(pos);
        self.report.warn(message, site, code);
    }

    // --- entry point --------------------------------------------------------

    fn run(&mut self, program: &Program) {
        collect_names(&program.body, &mut self.mutated, &mut self.overloaded);
        self.load_modules(&program.body);
        self.push_scope();
        self.hoist(&program.body);
        self.stmts(&program.body);
        self.unused_privates();
        self.pop_scope();
    }

    /// Warn about a private name nothing in the file reads (§11).
    fn unused_privates(&mut self) {
        let unused: Vec<(String, Pos)> = self
            .scopes
            .last()
            .map(|scope| {
                scope
                    .names
                    .iter()
                    .filter(|(name, _)| is_private(name) && !self.read.contains(*name))
                    .map(|(name, binding)| (name.clone(), binding.pos))
                    .collect()
            })
            .unwrap_or_default();
        let mut unused = unused;
        unused.sort_by_key(|(_, pos)| *pos);
        for (name, pos) in unused {
            self.warn(format!("`{name}` is private and never used"), pos, "unused-private");
        }
    }

    // --- modules ------------------------------------------------------------

    fn load_modules(&mut self, body: &[Stmt]) {
        let mut uses: Vec<(String, Pos)> = Vec::new();
        collect_uses(body, &mut uses);
        for (name, pos) in uses {
            let Some(path) = self.resolve_module(&name) else {
                self.names_are_knowable = false;
                self.warn(
                    format!(
                        "cannot find module `{name}`: name resolution is switched off for this file"
                    ),
                    pos,
                    "unresolved-module",
                );
                continue;
            };
            let exports = match module_exports(&path) {
                Some(exports) => exports,
                None => {
                    self.names_are_knowable = false;
                    self.warn(
                        format!("cannot parse module `{name}`; name resolution is switched off"),
                        pos,
                        "unresolved-module",
                    );
                    continue;
                }
            };
            // Silent shadowing is the failure mode that reaches production,
            // because the wrong `decode` usually still returns something (§11).
            let clashes: Vec<String> = exports
                .keys()
                .filter(|n| self.imports.contains_key(*n))
                .filter(|n| self.imports.get(*n).map(|(m, _)| m != &name).unwrap_or(false))
                .cloned()
                .collect();
            let mut clashes = clashes;
            clashes.sort();
            for clash in clashes {
                let (other, _) = self.imports[&clash].clone();
                self.warn(
                    format!(
                        "`{clash}` is exported by both `{other}` and `{name}`; \
                         the unqualified name now means `{name}::{clash}` — write `{name}::{clash}` \
                         or `{other}::{clash}` to say which"
                    ),
                    pos,
                    "ambiguous-import",
                );
            }
            let mut shadowed: Vec<String> =
                exports.keys().filter(|n| Native::lookup(n).is_some()).cloned().collect();
            shadowed.sort();
            for builtin in shadowed {
                self.warn(
                    format!(
                        "`{name}` exports `{builtin}`, which is also a builtin; \
                         the unqualified name now means `{name}::{builtin}` — \
                         write `::{builtin}` for the builtin"
                    ),
                    pos,
                    "shadowed-builtin",
                );
            }
            for (export, arity) in &exports {
                self.imports.insert(export.clone(), (name.clone(), arity.clone()));
            }
            self.modules.insert(name.clone(), ModuleInfo { exports });
        }
    }

    fn resolve_module(&self, name: &str) -> Option<PathBuf> {
        let filename = format!("{name}.hy");
        let mut dirs = vec![self.dir.clone()];
        dirs.extend(self.search_path.iter().cloned());
        dirs.into_iter().map(|d| d.join(&filename)).find(|p| p.is_file())
    }

    // --- scopes -------------------------------------------------------------

    fn push_scope(&mut self) {
        self.scopes.push(CheckScope::default());
    }

    fn pop_scope(&mut self) -> CheckScope {
        self.scopes.pop().expect("a scope to pop")
    }

    fn declare(&mut self, name: &str, binding: Binding) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.names.insert(name.to_string(), binding);
        }
    }

    fn lookup(&self, name: &str) -> Option<&Binding> {
        self.scopes.iter().rev().find_map(|scope| scope.names.get(name))
    }

    fn is_bound(&self, name: &str) -> bool {
        self.lookup(name).is_some()
            || self.imports.contains_key(name)
            || self.externs.contains(name)
            || Native::lookup(name).is_some()
    }

    fn trail_local_pos(&self, name: &str) -> Option<Pos> {
        self.scopes.iter().rev().find_map(|scope| scope.trail_locals.get(name).copied())
    }

    /// Names a block declares are visible to everything in it, so that
    /// `fn a() b() end` before `fn b()` is not reported.
    fn hoist(&mut self, body: &[Stmt]) {
        for stmt in body {
            match stmt {
                Stmt::FnDecl { name, def, pos } => self.declare(
                    name,
                    Binding { pos: *pos, arity: Some(Signature::of(def, name)), keys: None },
                ),
                Stmt::Decl { names, value, pos } => {
                    for (i, name) in names.iter().enumerate() {
                        // Only the first name takes the value's shape: the rest
                        // are additional information (channels §6.2).
                        let binding = match i {
                            0 => self.binding_for(name, value, *pos),
                            _ => Binding { pos: *pos, arity: None, keys: None },
                        };
                        self.declare(name, binding);
                    }
                }
                _ => {}
            }
        }
    }

    fn binding_for(&self, name: &str, value: &Expr, pos: Pos) -> Binding {
        let mut binding = Binding { pos, arity: None, keys: None };
        if self.mutated.contains(name) {
            return binding;
        }
        match value {
            Expr::Closure(def) => binding.arity = Some(Signature::of(def, name)),
            Expr::Dict { entries, .. } if entries.iter().all(|(k, _)| k.is_static()) => {
                binding.keys = Some(entries.iter().map(|(k, _)| k.name.clone()).collect())
            }
            _ => {}
        }
        binding
    }

    // --- statements ---------------------------------------------------------

    fn stmts(&mut self, body: &[Stmt]) {
        for stmt in body {
            self.stmt(stmt);
        }
    }

    fn scoped(&mut self, body: &[Stmt]) -> CheckScope {
        self.push_scope();
        self.hoist(body);
        self.stmts(body);
        self.pop_scope()
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Use { .. } => {}
            Stmt::FnDecl { name, def, pos } => {
                self.declare(
                    name,
                    Binding { pos: *pos, arity: Some(Signature::of(def, name)), keys: None },
                );
                self.closure(def);
            }
            Stmt::Decl { names, value, pos } => {
                self.expr(value);
                self.multi_value(names.len(), value, *pos);
                for (i, name) in names.iter().enumerate() {
                    let binding = match i {
                        0 => self.binding_for(name, value, *pos),
                        _ => Binding { pos: *pos, arity: None, keys: None },
                    };
                    self.declare(name, binding);
                }
            }
            Stmt::Assign { targets, op, value, pos } => {
                self.expr(value);
                self.multi_value(targets.len(), value, *pos);
                for target in targets {
                    // `a += 1` reads `a` as well as writing it, so a private
                    // name that is only ever incremented is used, not unused
                    // (§11).
                    if op.is_some() {
                        if let Some(Expr::Name { name, .. }) = target.lvalue_root() {
                            self.read.insert(name.clone());
                        }
                    }
                    self.assign_target(target, *pos);
                }
            }
            Stmt::Expr { expr, pos } => {
                // A required side effect inside a racing trail may never run,
                // because a trail is not guaranteed to start (§9.1, §11).
                if self.race_depth > 0 && self.trail_depth > 0 && matches!(expr, Expr::Call { .. }) {
                    self.warn(
                        "this call is a side effect inside a racing trail: \
                         a losing trail's result is discarded and a trail may never start at all",
                        *pos,
                        "effect-in-race",
                    );
                }
                self.expr(expr);
            }
            Stmt::Return { values, pos } => {
                if self.trail_depth > 0 {
                    self.error(
                        "`return` inside a trail is not allowed; \
                         `break` ends the trail, and a value has nowhere to return to",
                        *pos,
                        "return-in-trail",
                    );
                }
                for value in values {
                    self.expr(value);
                }
            }
            Stmt::If { branches, .. } => {
                for branch in branches {
                    if let Some(cond) = &branch.cond {
                        self.expr(cond);
                    }
                    // `else` followed by a lone nested `if` is probably a
                    // mis-spelled `else if` (§11).
                    if branch.keyword == "else" && branch.body.len() == 1 {
                        if let Stmt::If { pos, .. } = &branch.body[0] {
                            if pos.line == branch.pos.line + 1 {
                                self.warn(
                                    "an `else` whose whole body is an `if`: \
                                     did you mean `else if`, which is one keyword and needs one `end`?",
                                    branch.pos,
                                    "else-then-if",
                                );
                            }
                        }
                    }
                    self.scoped(&branch.body);
                }
            }
            Stmt::While { cond, body, label, .. } => {
                self.expr(cond);
                self.labels.push(Label { name: label.clone(), kind: LabelKind::Loop });
                self.scoped(body);
                self.labels.pop();
            }
            Stmt::For { var, iterable, body, label, pos, .. } => {
                self.expr(iterable);
                self.labels.push(Label { name: label.clone(), kind: LabelKind::Loop });
                self.push_scope();
                self.declare(var, Binding { pos: *pos, arity: None, keys: None });
                self.hoist(body);
                self.stmts(body);
                self.pop_scope();
                self.labels.pop();
            }
            Stmt::Break { target, pos } => self.check_break(target, *pos),
            Stmt::Continue { label, pos } => self.check_continue(label.as_deref(), *pos),
            Stmt::Parallel { kind, trails, rows, label, pos, .. } => {
                self.check_split_header(rows, *pos);
                let mut declared: Vec<(String, Pos)> = Vec::new();
                for trail in trails {
                    self.trail_shape = Some((trail.column, trails.len()));
                    let scope = self.trail_body(*kind, &trail.body, label.clone());
                    self.trail_shape = None;
                    for (name, binding) in scope.names {
                        declared.push((name, binding.pos));
                    }
                }
                self.record_trail_locals(declared);
            }
            Stmt::ParallelFor { kind, var, iterable, body, label, pos, .. } => {
                self.expr(iterable);
                self.push_scope();
                self.declare(var, Binding { pos: *pos, arity: None, keys: None });
                self.labels.push(Label { name: label.clone(), kind: LabelKind::Trail });
                self.trail_depth += 1;
                if *kind == BlockKind::Race {
                    self.race_depth += 1;
                }
                self.hoist(body);
                self.stmts(body);
                if *kind == BlockKind::Race {
                    self.race_depth -= 1;
                }
                self.trail_depth -= 1;
                self.labels.pop();
                let scope = self.pop_scope();
                let declared: Vec<(String, Pos)> = scope
                    .names
                    .into_iter()
                    .filter(|(name, _)| name != var)
                    .map(|(name, b)| (name, b.pos))
                    .collect();
                self.record_trail_locals(declared);
            }
            Stmt::ParallelWhile { kind, cond, body, label, .. } => {
                self.expr(cond);
                let scope = self.trail_body(*kind, body, label.clone());
                let declared: Vec<(String, Pos)> =
                    scope.names.into_iter().map(|(name, b)| (name, b.pos)).collect();
                self.record_trail_locals(declared);
            }
        }
    }

    fn trail_body(&mut self, kind: BlockKind, body: &[Stmt], label: Option<String>) -> CheckScope {
        self.labels.push(Label { name: label, kind: LabelKind::Trail });
        self.trail_depth += 1;
        if kind == BlockKind::Race {
            self.race_depth += 1;
        }
        let scope = self.scoped(body);
        if kind == BlockKind::Race {
            self.race_depth -= 1;
        }
        self.trail_depth -= 1;
        self.labels.pop();
        scope
    }

    /// Remember what the block's trails declared, so a read after the block
    /// gets the diagnostic §11 calls the best catch in the language.
    fn record_trail_locals(&mut self, declared: Vec<(String, Pos)>) {
        if let Some(scope) = self.scopes.last_mut() {
            for (name, pos) in declared {
                scope.trail_locals.insert(name, pos);
            }
        }
    }

    /// `parallel` on one line and `for` on the next is a compound keyword split
    /// across lines, which quietly means something else entirely (§2, §11).
    fn check_split_header(&mut self, rows: &[Row], pos: Pos) {
        let Some(first) = rows.first() else { return };
        let Some(cell) = first.cells.first() else { return };
        let Some(token) = cell.tokens.first() else { return };
        if let Tok::Kw(kw) = &token.kind {
            if (*kw == "for" || *kw == "while") && first.cells.len() == 1 {
                self.error(
                    format!(
                        "the compound keyword ends up split across lines: \
                         `{kw}` on the line after a block header is a separate statement. \
                         Write `parallel {kw}` or `race {kw}` on one line"
                    ),
                    pos,
                    "split-compound-keyword",
                );
            }
        }
    }

    fn check_break(&mut self, target: &BreakTarget, pos: Pos) {
        match target {
            BreakTarget::Trail => {
                if !self.labels.iter().any(|l| l.kind == LabelKind::Trail) {
                    self.error(
                        "`break trail` is only meaningful inside a trail",
                        pos,
                        "break-outside-trail",
                    );
                }
            }
            BreakTarget::Innermost => {
                let has_loop = self.labels.iter().any(|l| l.kind == LabelKind::Loop);
                let in_trail = self.labels.iter().any(|l| l.kind == LabelKind::Trail);
                if !has_loop && !in_trail {
                    self.error("`break` outside any loop", pos, "break-outside-loop");
                }
            }
            BreakTarget::Label(name) => {
                if !self.labels.iter().any(|l| l.name.as_deref() == Some(name.as_str())) {
                    self.error(
                        format!("no loop or block labelled `{name}` is in scope"),
                        pos,
                        "unknown-label",
                    );
                }
            }
        }
    }

    fn check_continue(&mut self, label: Option<&str>, pos: Pos) {
        match label {
            None => {
                if !self.labels.iter().any(|l| l.kind == LabelKind::Loop) {
                    self.error("`continue` outside any loop", pos, "continue-outside-loop");
                }
            }
            Some(name) => {
                match self.labels.iter().find(|l| l.name.as_deref() == Some(name)) {
                    Some(label) if label.kind == LabelKind::Loop => {}
                    Some(_) => self.error(
                        format!("`{name}` labels a block, not a loop, so it has no next iteration"),
                        pos,
                        "continue-block-label",
                    ),
                    None => self.error(
                        format!("no loop labelled `{name}` is in scope"),
                        pos,
                        "unknown-label",
                    ),
                }
            }
        }
    }

    // --- assignment targets -------------------------------------------------

    fn assign_target(&mut self, target: &Expr, pos: Pos) {
        match target {
            Expr::Name { name, .. } => {
                if self.is_bound(name) || !self.names_are_knowable {
                    return;
                }
                if let Some(declared_at) = self.trail_local_pos(name) {
                    self.error(
                        format!(
                            "`{name}` was declared with `:=` inside a trail (line {}), \
                             so it is gone at the join; declare it above the block \
                             and assign to it with `=` inside",
                            declared_at.line
                        ),
                        pos,
                        "trail-local",
                    );
                    return;
                }
                self.error(
                    format!("`{name}` has no binding in any enclosing scope; `:=` declares it"),
                    pos,
                    "assign-undeclared",
                );
            }
            Expr::Namespace { module, name, .. } => self.namespace(module, name, pos),
            Expr::Key { obj, .. } | Expr::Index { obj, .. } => {
                if let Expr::Index { index, .. } = target {
                    self.expr(index);
                }
                self.assign_target(obj, pos);
            }
            other => self.expr(other),
        }
    }

    // --- expressions --------------------------------------------------------

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Num { .. } => {}
            // §11: an undeclared name used inside a `\(…)` interpolation is a
            // hard error like any other, which is why this walks the parts.
            Expr::Str { parts, .. } => self.str_parts(parts),
            Expr::Sym(sym) => self.str_parts(&sym.parts),
            Expr::List { items, .. } => {
                for item in items {
                    self.expr(item);
                }
            }
            Expr::Dict { entries, pos } => {
                let mut seen: HashMap<&str, Pos> = HashMap::new();
                for (key, value) in entries {
                    self.str_parts(&key.parts);
                    // A key built by interpolation is not known here, so it
                    // cannot be a *provable* duplicate.
                    if !key.is_static() {
                        self.expr(value);
                        continue;
                    }
                    if let Some(first) = seen.insert(&key.name, key.pos) {
                        self.error(
                            format!(
                                "duplicate key `.{}` in one dict literal (first at line {})",
                                key.name, first.line
                            ),
                            key.pos,
                            "duplicate-key",
                        );
                    }
                    self.expr(value);
                }
                let _ = pos;
            }
            Expr::Name { name, pos } => {
                self.read.insert(name.clone());
                if self.is_bound(name) || !self.names_are_knowable {
                    return;
                }
                if let Some(declared_at) = self.trail_local_pos(name) {
                    self.error(
                        format!(
                            "`{name}` was declared with `:=` inside a trail (line {}), \
                             so it does not exist after the block",
                            declared_at.line
                        ),
                        *pos,
                        "trail-local",
                    );
                    return;
                }
                self.error(format!("`{name}` is not declared"), *pos, "undeclared-name");
            }
            Expr::Namespace { module, name, pos } => self.namespace(module, name, *pos),
            Expr::Key { obj, key, pos } => {
                self.expr(obj);
                self.str_parts(&key.parts);
                if key.is_static() {
                    self.check_known_key(obj, &key.name, *pos);
                }
            }
            Expr::Index { obj, index, pos } => {
                self.expr(obj);
                self.expr(index);
                if let Expr::Sym(sym) = index.as_ref() {
                    if sym.is_static() {
                        self.check_known_key(obj, &sym.name, *pos);
                    }
                }
            }
            Expr::Call { callee, args, pos } => {
                self.expr(callee);
                for arg in args {
                    self.expr(&arg.value);
                }
                self.check_call(callee, args, *pos);
            }
            Expr::Unary { operand, .. } => self.expr(operand),
            Expr::Binary { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            Expr::Ref { target, pos } => {
                if !target.is_lvalue() {
                    self.error(
                        "`&` takes a variable, a dict key or a list element",
                        *pos,
                        "ref-not-lvalue",
                    );
                }
                if target.lvalue_root().is_none() {
                    self.error(
                        "a `&` must be rooted at a name: there is nothing to alias otherwise",
                        *pos,
                        "ref-not-lvalue",
                    );
                }
                // The only way to share mutable data between trails, and worth
                // a second look every time (§9.2, §11).
                if self.trail_depth > 0 {
                    self.warn(
                        "a `&` reference inside a trail: this is shared mutable state, \
                         and the only thing two trails can race on besides the parent scope",
                        *pos,
                        "ref-into-trail",
                    );
                }
                self.expr(target);
            }
            Expr::Closure(def) => self.closure(def),
        }
    }

    /// Walk the expressions inside a string or symbol's interpolations.
    fn str_parts(&mut self, parts: &[StrPart]) {
        for part in parts {
            if let StrPart::Expr(expr) = part {
                self.expr(expr);
            }
        }
    }

    /// `a, b := f()` — only a call answers with several values, and naming more
    /// than it can answer with is a crash (channels §6.2).
    fn multi_value(&mut self, named: usize, value: &Expr, pos: Pos) {
        if named < 2 {
            return;
        }
        let Expr::Call { callee, .. } = value else {
            self.error(
                "only a call answers with several values, and a multi-value is not a value",
                pos,
                "multi-value-not-a-call",
            );
            return;
        };
        let Some(signature) = self.callee_signature(callee) else { return };
        if named > signature.returns {
            self.error(
                format!(
                    "`{}` answers with {} value{}, but {named} were named",
                    signature.label,
                    signature.returns,
                    if signature.returns == 1 { "" } else { "s" }
                ),
                pos,
                "too-many-values-named",
            );
        }
    }

    /// The signature behind a callee, where exactly one candidate is knowable.
    fn callee_signature(&self, callee: &Expr) -> Option<Signature> {
        match callee {
            Expr::Name { name, .. } => {
                if self.mutated.contains(name) || self.overloaded.contains(name) {
                    return None;
                }
                let local = self.lookup(name);
                let has_import = self.imports.contains_key(name);
                let has_native = self.names_are_knowable && Native::lookup(name).is_some();
                if usize::from(local.is_some()) + usize::from(has_import) + usize::from(has_native)
                    > 1
                {
                    return None;
                }
                match (local, has_import) {
                    (Some(binding), _) => binding.arity.clone(),
                    (None, true) => self.imports.get(name).and_then(|(_, a)| a.clone()),
                    (None, false) => Native::lookup(name).map(Signature::native),
                }
            }
            Expr::Namespace { module, name, .. } if module.is_empty() => {
                Native::lookup(name).map(Signature::native)
            }
            Expr::Namespace { module, name, .. } => {
                self.modules.get(module).and_then(|m| m.exports.get(name).cloned()).flatten()
            }
            _ => None,
        }
    }

    /// `send`, `receive` and `channel` are lexically scoped to a `parallel` or
    /// `race` body: they name that block's trails, and there is nothing for
    /// them to name anywhere else. A function a trail calls is *not* inside it
    /// — that is the deliberate contrast with `alive()`, which is dynamic at
    /// any depth (channels §6.4).
    fn channel_call(&mut self, name: &str, args: &[Arg], pos: Pos) {
        if !self.names_are_knowable
            || self.lookup(name).is_some()
            || self.imports.contains_key(name)
            || !CHANNEL_NATIVES.iter().any(|n| n.name() == name)
        {
            return;
        }
        if self.trail_depth == 0 {
            self.error(
                format!(
                    "`{name}` belongs inside a `parallel` or `race` block: it names that \
                     block's trails, and a function a trail calls is not inside it"
                ),
                pos,
                "channel-outside-trail",
            );
            return;
        }
        // Only the row form knows how many trails it has before it runs.
        let Some((me, count)) = self.trail_shape else { return };
        // `send`'s first argument is the value; the rest are indices, as all of
        // `receive`'s are.
        let skip = usize::from(name == Native::Send.name());
        for arg in args.iter().filter(|a| a.name.is_none()).skip(skip) {
            let Expr::Num { value, pos, .. } = &arg.value else { continue };
            let index = *value as usize;
            if value.fract() != 0.0 || *value < 0.0 {
                continue;
            }
            if index == me {
                self.error(
                    format!("trail {me} is this one: a trail names its siblings, not itself"),
                    *pos,
                    "channel-is-self",
                );
            } else if index >= count {
                self.error(
                    format!(
                        "no trail {index}: this block has {count} trail{}, 0 to {}",
                        if count == 1 { "" } else { "s" },
                        count.saturating_sub(1)
                    ),
                    *pos,
                    "no-such-channel",
                );
            }
        }
    }

    fn closure(&mut self, def: &ClosureDef) {
        // A function body is not a trail body: `return` is fine in it, and
        // `break trail` is not (QUESTIONS.md §16).
        let trail_depth = std::mem::take(&mut self.trail_depth);
        let race_depth = std::mem::take(&mut self.race_depth);
        let trail_shape = std::mem::take(&mut self.trail_shape);
        let labels = std::mem::take(&mut self.labels);

        self.push_scope();
        for param in &def.params {
            if let Some(default) = &param.default {
                self.expr(default);
            }
            self.declare(&param.name, Binding { pos: param.pos, arity: None, keys: None });
        }
        match &def.body {
            ClosureBody::Expr(expr) => self.expr(expr),
            ClosureBody::Block(body) => {
                self.hoist(body);
                self.stmts(body);
            }
        }
        self.pop_scope();

        self.labels = labels;
        self.trail_shape = trail_shape;
        self.race_depth = race_depth;
        self.trail_depth = trail_depth;
    }

    fn namespace(&mut self, module: &str, name: &str, pos: Pos) {
        // `::name` is the language's own namespace, which is always knowable —
        // that is the point of writing it (§7).
        if module.is_empty() {
            if Native::lookup(name).is_none() {
                self.error(format!("there is no builtin named `{name}`"), pos, "unknown-builtin");
            }
            return;
        }
        // Private names are not reachable through `::` (§7). That is a
        // syntactic fact, so it holds even when modules cannot be resolved.
        if is_private(name) {
            self.error(
                format!("`{name}` is private to `{module}` and is not reachable through `::`"),
                pos,
                "private-through-namespace",
            );
            return;
        }
        if !self.names_are_knowable {
            return;
        }
        match self.modules.get(module) {
            None => self.error(
                format!("no module `{module}` is in scope; add `use {module}`"),
                pos,
                "unknown-module",
            ),
            Some(info) if !info.exports.contains_key(name) => self.error(
                format!("module `{module}` does not export `{name}`"),
                pos,
                "unknown-export",
            ),
            Some(_) => {}
        }
    }

    /// A key read on a dict literal that provably lacks the key (§11).
    fn check_known_key(&mut self, obj: &Expr, key: &str, pos: Pos) {
        let keys: Option<Vec<String>> = match obj {
            Expr::Dict { entries, .. } if entries.iter().all(|(k, _)| k.is_static()) => {
                Some(entries.iter().map(|(k, _)| k.name.clone()).collect())
            }
            Expr::Name { name, .. } => self.lookup(name).and_then(|b| b.keys.clone()),
            _ => None,
        };
        let Some(keys) = keys else { return };
        if keys.iter().any(|k| k == key) {
            return;
        }
        let known = if keys.is_empty() {
            "it has no keys".to_string()
        } else {
            format!("it has {}", keys.iter().map(|k| format!(".{k}")).collect::<Vec<_>>().join(", "))
        };
        self.error(
            format!("this dict has no key `.{key}` — {known}, and reading a missing key crashes"),
            pos,
            "missing-key",
        );
    }

    /// A call nothing accepts, and a missing `&` on the one that does
    /// (§11, §5.1, §3).
    fn check_call(&mut self, callee: &Expr, args: &[Arg], pos: Pos) {
        match callee {
            Expr::Name { name, .. } => self.channel_call(name, args, pos),
            Expr::Namespace { module, name, .. } if module.is_empty() => {
                self.channel_call(name, args, pos)
            }
            _ => {}
        }
        let (name, signature) = match callee {
            Expr::Name { name, .. } => {
                let local = self.lookup(name).and_then(|b| b.arity.clone());
                let has_local = self.lookup(name).is_some();
                let has_import = self.imports.contains_key(name);
                // Only when every `use` resolved: an unresolvable module could
                // export a `push` of its own, and §11 reports what is
                // guaranteed, not what is likely.
                let has_native = self.names_are_knowable && Native::lookup(name).is_some();

                // With more than one candidate, a call this one rejects simply
                // goes to the next (§3), so nothing here is guaranteed.
                if usize::from(has_local) + usize::from(has_import) + usize::from(has_native) > 1 {
                    return;
                }
                let known = if has_local || has_import {
                    local.or_else(|| self.imports.get(name).and_then(|(_, a)| a.clone()))
                } else if has_native {
                    Native::lookup(name).map(Signature::native)
                } else {
                    None
                };
                (name.clone(), known)
            }
            // `::name` names the builtin unambiguously, so its signature is
            // known even when a module could not be resolved.
            Expr::Namespace { module, name, .. } if module.is_empty() => {
                (format!("::{name}"), Native::lookup(name).map(Signature::native))
            }
            Expr::Namespace { module, name, .. } => {
                let signature = self
                    .modules
                    .get(module)
                    .and_then(|m| m.exports.get(name).cloned())
                    .flatten();
                (format!("{module}::{name}"), signature)
            }
            _ => return,
        };
        let Some(signature) = signature else { return };
        // A name that is written to could hold anything by the time it is
        // called, and a shadowed one has candidates this does not model.
        if self.mutated.contains(&name) || self.overloaded.contains(&name) {
            return;
        }

        let Some(bound) = signature.bind(args) else {
            self.error(
                format!(
                    "`{name}` does not accept this call: it is `{}`",
                    signature.label
                ),
                pos,
                "no-matching-call",
            );
            return;
        };

        // A by-reference parameter passed by value is a guaranteed crash — and
        // before it was one, it was a silent no-op (§5.1).
        for (index, arg) in bound.iter().enumerate() {
            let Some(arg) = arg else { continue };
            if signature.by_ref.get(index) == Some(&true)
                && !matches!(arg.value, Expr::Ref { .. })
            {
                self.error(
                    format!(
                        "`{}` takes `{}` by reference; write `&` before it, \
                         or it is handed a copy",
                        signature.label,
                        signature.names.get(index).cloned().unwrap_or_default()
                    ),
                    arg.pos,
                    "missing-reference",
                );
            }
        }
    }
}

// --- pre-passes -------------------------------------------------------------

/// Two coarse, file-wide sets: names that are written to or `&`-referenced, and
/// names declared more than once.
///
/// Value semantics are what make the first worth doing: passing a value to a
/// function cannot change it, so only those two things can move a binding out
/// from under an assumption. The second is about resolution — a name declared
/// twice has two candidates, and a call one rejects goes to the other (§3).
fn collect_names(body: &[Stmt], out: &mut HashSet<String>, overloaded: &mut HashSet<String>) {
    let mut declared: HashSet<String> = HashSet::new();
    walk_stmts(body, &mut |stmt| match stmt {
        Stmt::Assign { targets, .. } => {
            for target in targets {
                if let Some(Expr::Name { name, .. }) = target.lvalue_root() {
                    out.insert(name.clone());
                }
            }
        }
        Stmt::Decl { names, .. } => {
            for name in names {
                if !declared.insert(name.clone()) {
                    overloaded.insert(name.clone());
                }
            }
        }
        Stmt::FnDecl { name, .. } if !declared.insert(name.clone()) => {
            overloaded.insert(name.clone());
        }
        _ => {}
    });
    walk_exprs(body, &mut |expr| {
        if let Expr::Ref { target, .. } = expr {
            if let Some(Expr::Name { name, .. }) = target.lvalue_root() {
                out.insert(name.clone());
            }
        }
    });
}

fn collect_uses(body: &[Stmt], out: &mut Vec<(String, Pos)>) {
    walk_stmts(body, &mut |stmt| {
        if let Stmt::Use { module, pos } = stmt {
            out.push((module.clone(), *pos));
        }
    });
}

/// A module's public toplevel names, with arities where they are functions.
fn module_exports(path: &Path) -> Option<HashMap<String, Option<Signature>>> {
    let src = std::fs::read_to_string(path).ok()?;
    let program = parse(&src, &path.display().to_string()).ok()?;
    let mut exports: HashMap<String, Option<Signature>> = HashMap::new();
    for stmt in &program.body {
        match stmt {
            Stmt::FnDecl { name, def, .. } if !is_private(name) => {
                exports.insert(name.clone(), Some(Signature::of(def, name)));
            }
            Stmt::Decl { names, value, .. } => {
                for (i, name) in names.iter().enumerate() {
                    if is_private(name) {
                        continue;
                    }
                    let signature = match (i, value) {
                        (0, Expr::Closure(def)) => Some(Signature::of(def, name)),
                        _ => None,
                    };
                    exports.insert(name.clone(), signature);
                }
            }
            _ => {}
        }
    }
    Some(exports)
}

// --- generic walks ----------------------------------------------------------

fn walk_stmts(body: &[Stmt], f: &mut impl FnMut(&Stmt)) {
    for stmt in body {
        f(stmt);
        match stmt {
            Stmt::If { branches, .. } => {
                for branch in branches {
                    walk_stmts(&branch.body, f);
                }
            }
            Stmt::While { body, .. }
            | Stmt::For { body, .. }
            | Stmt::ParallelFor { body, .. }
            | Stmt::ParallelWhile { body, .. } => walk_stmts(body, f),
            Stmt::Parallel { trails, .. } => {
                for trail in trails {
                    walk_stmts(&trail.body, f);
                }
            }
            Stmt::FnDecl { def, .. } => {
                if let ClosureBody::Block(body) = &def.body {
                    walk_stmts(body, f);
                }
            }
            _ => {}
        }
        walk_stmt_exprs(stmt, &mut |expr| walk_closure_stmts(expr, f));
    }
}

fn walk_closure_stmts(expr: &Expr, f: &mut impl FnMut(&Stmt)) {
    if let Expr::Closure(def) = expr {
        if let ClosureBody::Block(body) = &def.body {
            walk_stmts(body, f);
        }
    }
}

fn walk_exprs(body: &[Stmt], f: &mut dyn FnMut(&Expr)) {
    walk_stmts(body, &mut |stmt| walk_stmt_exprs(stmt, &mut |expr| walk_expr(expr, f)));
}

fn walk_stmt_exprs(stmt: &Stmt, f: &mut impl FnMut(&Expr)) {
    match stmt {
        Stmt::Decl { value, .. } => f(value),
        Stmt::Assign { targets, value, .. } => {
            for target in targets {
                f(target);
            }
            f(value);
        }
        Stmt::Expr { expr, .. } => f(expr),
        Stmt::Return { values, .. } => {
            for value in values {
                f(value);
            }
        }
        Stmt::If { branches, .. } => {
            for branch in branches {
                if let Some(cond) = &branch.cond {
                    f(cond);
                }
            }
        }
        Stmt::While { cond, .. } | Stmt::ParallelWhile { cond, .. } => f(cond),
        Stmt::For { iterable, .. } | Stmt::ParallelFor { iterable, .. } => f(iterable),
        Stmt::FnDecl { def, .. } => {
            if let ClosureBody::Expr(expr) = &def.body {
                f(expr);
            }
        }
        _ => {}
    }
}

/// A `dyn` callback rather than a generic one: an interpolation holds
/// expressions, so this recurses through itself and a generic closure would
/// monomorphise without bound.
fn walk_expr(expr: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(expr);
    fn walk_parts(parts: &[StrPart], f: &mut dyn FnMut(&Expr)) {
        for part in parts {
            if let StrPart::Expr(inner) = part {
                walk_expr(inner, f);
            }
        }
    }
    match expr {
        Expr::Str { parts, .. } => walk_parts(parts, f),
        Expr::Sym(sym) => walk_parts(&sym.parts, f),
        Expr::List { items, .. } => {
            for item in items {
                walk_expr(item, f);
            }
        }
        Expr::Dict { entries, .. } => {
            for (key, value) in entries {
                walk_parts(&key.parts, f);
                walk_expr(value, f);
            }
        }
        Expr::Key { obj, key, .. } => {
            walk_expr(obj, f);
            walk_parts(&key.parts, f);
        }
        Expr::Index { obj, index, .. } => {
            walk_expr(obj, f);
            walk_expr(index, f);
        }
        Expr::Call { callee, args, .. } => {
            walk_expr(callee, f);
            for arg in args {
                walk_expr(&arg.value, f);
            }
        }
        Expr::Unary { operand, .. } => walk_expr(operand, f),
        Expr::Ref { target, .. } => walk_expr(target, f),
        Expr::Binary { left, right, .. } => {
            walk_expr(left, f);
            walk_expr(right, f);
        }
        Expr::Closure(def) => {
            if let ClosureBody::Expr(inner) = &def.body {
                walk_expr(inner, f);
            }
        }
        _ => {}
    }
}
