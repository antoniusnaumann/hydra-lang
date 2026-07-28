//! The syntax tree (spec §3).
//!
//! Block nodes keep the position of their `end` as well as of their header,
//! because the formatter may never move a line break (§12 rule 4) and so needs
//! to know which source line every construct occupies.

use std::collections::HashMap;
use std::rc::Rc;

use crate::errors::Pos;
use crate::lexer::Token;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Parallel,
    Race,
}

impl BlockKind {
    pub fn keyword(self) -> &'static str {
        match self {
            BlockKind::Parallel => "parallel",
            BlockKind::Race => "race",
        }
    }
}

/// One piece of a string literal or a quoted symbol.
#[derive(Clone, Debug)]
pub enum StrPart {
    Text(String),
    /// A `\(…)` interpolation (§1). The expression was lexed and parsed
    /// recursively, so it can be anything, including further strings.
    Expr(Expr),
}

/// `.name`, `."not an identifier"`, or `."\(prefix)-id"` (§2).
///
/// `name` is the literal text when the symbol does not interpolate; when it
/// does, the symbol is built at run time and `name` is empty.
#[derive(Clone, Debug)]
pub struct SymLit {
    pub name: String,
    pub quoted: bool,
    pub parts: Vec<StrPart>,
    pub pos: Pos,
}

impl SymLit {
    /// A symbol known at compile time. Interpolation is what gives the language
    /// dynamic symbol construction without a `sym(str)` builtin (§2).
    pub fn is_static(&self) -> bool {
        !self.parts.iter().any(|p| matches!(p, StrPart::Expr(_)))
    }

    pub fn plain(name: impl Into<String>, quoted: bool, pos: Pos) -> SymLit {
        let name = name.into();
        SymLit { parts: vec![StrPart::Text(name.clone())], name, quoted, pos }
    }
}

#[derive(Clone, Debug)]
pub enum Expr {
    Num { value: f64, raw: String, pos: Pos },
    Str { parts: Vec<StrPart>, pos: Pos },
    Sym(SymLit),
    List { items: Vec<Expr>, pos: Pos },
    Dict { entries: Vec<(SymLit, Expr)>, pos: Pos },
    Name { name: String, pos: Pos },
    /// `mod::name`, and `::name` with an empty module — the language's own
    /// namespace, which is how a builtin is reached past a shadow (§7).
    Namespace { module: String, name: String, pos: Pos },
    /// `a.b` — sugar for `a[.b]` (§5).
    Key { obj: Box<Expr>, key: SymLit, pos: Pos },
    Index { obj: Box<Expr>, index: Box<Expr>, pos: Pos },
    Call { callee: Box<Expr>, args: Vec<Arg>, pos: Pos },
    /// `-x`, `~x`, `not x`.
    Unary { op: &'static str, operand: Box<Expr>, pos: Pos },
    /// `&lvalue` (§5.1): the caller marks it, never the callee.
    Ref { target: Box<Expr>, pos: Pos },
    Binary { op: &'static str, left: Box<Expr>, right: Box<Expr>, pos: Pos },
    Closure(Rc<ClosureDef>),
}

impl Expr {
    pub fn pos(&self) -> Pos {
        match self {
            Expr::Num { pos, .. }
            | Expr::Str { pos, .. }
            | Expr::List { pos, .. }
            | Expr::Dict { pos, .. }
            | Expr::Name { pos, .. }
            | Expr::Namespace { pos, .. }
            | Expr::Key { pos, .. }
            | Expr::Index { pos, .. }
            | Expr::Call { pos, .. }
            | Expr::Unary { pos, .. }
            | Expr::Ref { pos, .. }
            | Expr::Binary { pos, .. } => *pos,
            Expr::Sym(s) => s.pos,
            Expr::Closure(c) => c.pos,
        }
    }

    /// Only a variable, a dict key or a list element may be assigned to or
    /// referenced with `&` (§5.1). A name selected out of a module is a
    /// variable too, so `mod::x = 1` is allowed (QUESTIONS.md §15).
    pub fn is_lvalue(&self) -> bool {
        matches!(
            self,
            Expr::Name { .. } | Expr::Key { .. } | Expr::Index { .. } | Expr::Namespace { .. }
        )
    }

    /// `::name`: a builtin, not a variable. It can be called and passed
    /// around, but never assigned to or referenced.
    pub fn is_builtin_ref(&self) -> bool {
        matches!(self, Expr::Namespace { module, .. } if module.is_empty())
    }

    /// The variable an lvalue is rooted at, if it is rooted at one at all.
    pub fn lvalue_root(&self) -> Option<&Expr> {
        match self {
            Expr::Namespace { module, .. } if module.is_empty() => None,
            Expr::Name { .. } | Expr::Namespace { .. } => Some(self),
            Expr::Key { obj, .. } | Expr::Index { obj, .. } => obj.lvalue_root(),
            _ => None,
        }
    }
}

/// One argument at a call site. `name = expr` names the parameter it fills;
/// named arguments come after the positional ones.
#[derive(Clone, Debug)]
pub struct Arg {
    pub name: Option<String>,
    pub value: Expr,
    pub pos: Pos,
}

impl Arg {
    pub fn positional(value: Expr) -> Arg {
        let pos = value.pos();
        Arg { name: None, value, pos }
    }
}

#[derive(Clone, Debug)]
pub enum ClosureBody {
    /// `fn(a, b) a + b` — decided by something following the `)` on the same
    /// line (§3).
    Expr(Box<Expr>),
    Block(Vec<Stmt>),
}

/// One parameter.
///
/// `&name` requires the *call* to pass a reference: the caller still writes the
/// `&`, so §5.1's "the caller marks it, never the callee" holds — the signature
/// only says that marking it is not optional. Without that, value semantics
/// make `push(rows, x)` a silent no-op that looks like working code.
///
/// `name = expr` gives the parameter a default, evaluated in the function's own
/// scope when the argument is missing. Parameters with defaults come after
/// those without, so positional filling stays unambiguous.
#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub by_ref: bool,
    pub default: Option<Expr>,
    pub pos: Pos,
}

impl Param {
    pub fn plain(name: impl Into<String>, pos: Pos) -> Param {
        Param { name: name.into(), by_ref: false, default: None, pos }
    }
}

#[derive(Clone, Debug)]
pub struct ClosureDef {
    pub name: String,
    pub params: Vec<Param>,
    pub body: ClosureBody,
    pub pos: Pos,
    pub end_pos: Pos,
}

impl ClosureDef {
    /// Parameters that must be supplied, which is every one before the first
    /// with a default.
    pub fn required(&self) -> usize {
        self.params.iter().filter(|p| p.default.is_none()).count()
    }
}

#[derive(Clone, Debug)]
pub struct Branch {
    /// `None` for the `else` clause.
    pub cond: Option<Expr>,
    pub body: Vec<Stmt>,
    pub pos: Pos,
    /// `if`, `else if` or `else` — kept so the formatter re-emits what was written.
    pub keyword: &'static str,
}

/// One cell of one row of a `parallel` block, kept as tokens for the formatter
/// (§12 rule 5) and for the column-wise parse (§4).
#[derive(Clone, Debug)]
pub struct Cell {
    pub tokens: Vec<Token>,
    pub pos: Pos,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub cells: Vec<Cell>,
    pub line: u32,
}

/// One column of a `parallel` block: the statements of one green thread.
#[derive(Clone, Debug)]
pub struct TrailDef {
    pub body: Vec<Stmt>,
    pub column: usize,
    pub pos: Pos,
}

#[derive(Clone, Debug)]
pub enum BreakTarget {
    /// `break` — innermost loop, or the trail when written directly in one.
    Innermost,
    /// `break trail` — the innermost trail, from any depth (§9.6).
    Trail,
    Label(String),
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Use {
        module: String,
        pos: Pos,
    },
    FnDecl {
        name: String,
        def: Rc<ClosureDef>,
        pos: Pos,
    },
    /// `x := expr` declares, shadowing an existing name (§6).
    Decl {
        name: String,
        value: Expr,
        pos: Pos,
    },
    /// `lvalue = expr` assigns to an existing binding (§6).
    Assign {
        target: Expr,
        value: Expr,
        pos: Pos,
    },
    If {
        branches: Vec<Branch>,
        pos: Pos,
        end_pos: Pos,
    },
    For {
        var: String,
        iterable: Expr,
        body: Vec<Stmt>,
        label: Option<String>,
        pos: Pos,
        end_pos: Pos,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
        label: Option<String>,
        pos: Pos,
        end_pos: Pos,
    },
    /// The row form: `parallel` / `race` with `||`-separated cells (§4).
    Parallel {
        kind: BlockKind,
        trails: Vec<TrailDef>,
        rows: Vec<Row>,
        label: Option<String>,
        pos: Pos,
        end_pos: Pos,
    },
    ParallelFor {
        kind: BlockKind,
        var: String,
        iterable: Expr,
        body: Vec<Stmt>,
        label: Option<String>,
        pos: Pos,
        end_pos: Pos,
    },
    ParallelWhile {
        kind: BlockKind,
        cond: Expr,
        body: Vec<Stmt>,
        label: Option<String>,
        pos: Pos,
        end_pos: Pos,
    },
    Break {
        target: BreakTarget,
        pos: Pos,
    },
    Continue {
        label: Option<String>,
        pos: Pos,
    },
    Return {
        value: Option<Expr>,
        pos: Pos,
    },
    Expr {
        expr: Expr,
        pos: Pos,
    },
}

impl Stmt {
    pub fn pos(&self) -> Pos {
        match self {
            Stmt::Use { pos, .. }
            | Stmt::FnDecl { pos, .. }
            | Stmt::Decl { pos, .. }
            | Stmt::Assign { pos, .. }
            | Stmt::If { pos, .. }
            | Stmt::For { pos, .. }
            | Stmt::While { pos, .. }
            | Stmt::Parallel { pos, .. }
            | Stmt::ParallelFor { pos, .. }
            | Stmt::ParallelWhile { pos, .. }
            | Stmt::Break { pos, .. }
            | Stmt::Continue { pos, .. }
            | Stmt::Return { pos, .. }
            | Stmt::Expr { pos, .. } => *pos,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Program {
    pub body: Vec<Stmt>,
    pub file: String,
    /// line -> (column, text), for the formatter only.
    pub comments: HashMap<u32, (u32, String)>,
    pub line_count: u32,
}
