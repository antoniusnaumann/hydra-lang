//! The formatter (spec §12).
//!
//! The formatter owns column padding and its output is the canonical form. The
//! rule that shapes the whole design is rule 4: preserve continuation boundaries,
//! because newlines and blank lines control continuation. Formatting is per
//! line: re-render its tokens with canonical spacing, and put it at the indent
//! its nesting level implies.
//!
//! Two consequences worth stating:
//!
//! * The indent comes from the *parse tree*, not from counting `end`s in the
//!   text, so a single-expression closure (`fn(a) a + 1`) does not open a block
//!   while a multi-line one does — exactly the distinction §3 draws.
//! * Cells of a `parallel` block are rendered from tokens and padded to the
//!   widest in their column (rule 5). A one-character edit therefore re-pads
//!   the block, which §12 accepts as the price of aligned columns.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::errors::Result;
use crate::lexer::{is_identifier, is_symbol_name, static_text, StrPiece, Tok, Token};
use crate::parser::parse_with_tokens;

/// Format a source file. The output is canonical and formatting it again
/// changes nothing (rule 6).
pub fn format_source(src: &str, file: &str) -> Result<String> {
    let (program, tokens) = parse_with_tokens(src, file)?;

    let line_count = src.lines().count().max(1) as u32;
    let mut lines: HashMap<u32, Vec<Token>> = HashMap::new();
    for token in &tokens {
        if matches!(token.kind, Tok::Newline | Tok::Eof) {
            continue;
        }
        lines.entry(token.pos.line).or_default().push(token.clone());
    }

    let mut f = Formatter {
        indent: vec![0; (line_count + 2) as usize],
        rows: HashMap::new(),
        statement_starts: HashSet::new(),
    };
    f.statements(&program.body, 0);

    // Only separate complete statements. A closure's `end` may be followed by
    // a call or operator continuing the same expression; a blank would change
    // its meaning. Parallel rows likewise share boundaries across columns.
    let significant: Vec<_> = tokens.iter().filter(|t| !t.is_newline() && !t.is_eof()).collect();
    let mut separate_after = HashSet::new();
    for pair in significant.windows(2) {
        let (end, next) = (pair[0], pair[1]);
        if end.is_kw("end") && next.pos.line > end.pos.line
            && !next.is_kw("end") && f.statement_starts.contains(&next.pos.line)
            && !f.rows.contains_key(&end.pos.line)
        {
            separate_after.insert(end.pos.line);
        }
    }

    let mut out = String::new();
    for line in 1..=line_count {
        let indent = f.indent.get(line as usize).copied().unwrap_or(0);
        let text = match f.rows.get(&line) {
            Some(row) => row.clone(),
            None => match lines.get(&line) {
                Some(tokens) => render(tokens),
                None => String::new(),
            },
        };
        let comment = program.comments.get(&line).map(|(_, text)| text.clone());

        let mut rendered = String::new();
        if !text.is_empty() || comment.is_some() {
            rendered.push_str(&"\t".repeat(indent));
        }
        rendered.push_str(&text);
        if let Some(comment) = comment {
            if !text.is_empty() {
                rendered.push(' ');
            }
            rendered.push_str(&comment);
        }
        while rendered.ends_with(' ') || rendered.ends_with('\t') {
            rendered.pop();
        }
        out.push_str(&rendered);
        out.push('\n');
        if separate_after.contains(&line)
            && (lines.contains_key(&(line + 1)) || program.comments.contains_key(&(line + 1)))
        {
            out.push('\n');
        }
    }
    Ok(out)
}

/// True when formatting would change the file.
pub fn is_formatted(src: &str, file: &str) -> Result<bool> {
    Ok(format_source(src, file)? == src)
}

struct Formatter {
    /// Indent level per source line.
    indent: Vec<usize>,
    /// Pre-rendered, column-padded rows of `parallel` blocks, by line.
    rows: HashMap<u32, String>,
    /// Lines starting independent statements or branches, safe to separate.
    statement_starts: HashSet<u32>,
}

impl Formatter {
    fn set(&mut self, line: u32, depth: usize) {
        if line == 0 {
            return;
        }
        if let Some(slot) = self.indent.get_mut(line as usize) {
            *slot = depth;
        }
    }

    /// Everything between a block's header and its `end` sits one level in,
    /// including the blank and comment-only lines nested statements never
    /// mention.
    fn fill(&mut self, from: u32, to: u32, depth: usize) {
        let mut line = from;
        while line <= to {
            self.set(line, depth);
            line += 1;
        }
    }

    fn statements(&mut self, body: &[Stmt], depth: usize) {
        for stmt in body {
            self.statement(stmt, depth);
        }
    }

    fn statement(&mut self, stmt: &Stmt, depth: usize) {
        self.statement_starts.insert(stmt.pos().line);
        self.set(stmt.pos().line, depth);
        match stmt {
            Stmt::If { branches, end_pos, .. } => {
                for branch in branches {
                    self.fill(branch.pos.line + 1, end_pos.line.saturating_sub(1), depth + 1);
                }
                for branch in branches {
                    self.statement_starts.insert(branch.pos.line);
                    self.statements(&branch.body, depth + 1);
                    self.set(branch.pos.line, depth);
                    if let Some(cond) = &branch.cond {
                        self.expr(cond, depth);
                    }
                }
                self.set(end_pos.line, depth);
            }
            Stmt::For { iterable, body, pos, end_pos, .. } => {
                self.block(body, *pos, *end_pos, depth);
                self.expr(iterable, depth);
            }
            Stmt::While { cond, body, pos, end_pos, .. } => {
                self.block(body, *pos, *end_pos, depth);
                self.expr(cond, depth);
            }
            Stmt::ParallelFor { iterable, body, pos, end_pos, .. } => {
                self.block(body, *pos, *end_pos, depth);
                self.expr(iterable, depth);
            }
            Stmt::ParallelWhile { cond, body, pos, end_pos, .. } => {
                self.block(body, *pos, *end_pos, depth);
                self.expr(cond, depth);
            }
            Stmt::FnDecl { def, pos, .. } => {
                self.fill(pos.line + 1, def.end_pos.line.saturating_sub(1), depth + 1);
                if let ClosureBody::Block(body) = &def.body {
                    self.statements(body, depth + 1);
                }
                self.set(def.end_pos.line, depth);
            }
            Stmt::Parallel { trails, rows, pos, end_pos, .. } => {
                self.fill(pos.line + 1, end_pos.line.saturating_sub(1), depth + 1);
                for trail in trails {
                    // A cell's own statements are rendered as part of its cell
                    // text, so they get no line of their own.
                    let _ = trail;
                }
                self.set(end_pos.line, depth);
                self.pad_rows(rows);
            }
            Stmt::Decl { value, .. } => self.expr(value, depth),
            Stmt::Assign { targets, value, .. } => {
                for target in targets {
                    self.expr(target, depth);
                }
                self.expr(value, depth);
            }
            Stmt::Expr { expr, .. } => self.expr(expr, depth),
            Stmt::Return { values, .. } => {
                for value in values {
                    self.expr(value, depth);
                }
            }
            _ => {}
        }
    }

    fn block(&mut self, body: &[Stmt], pos: crate::errors::Pos, end_pos: crate::errors::Pos, depth: usize) {
        self.fill(pos.line + 1, end_pos.line.saturating_sub(1), depth + 1);
        self.statements(body, depth + 1);
        self.set(end_pos.line, depth);
    }

    /// A multi-line closure opens a block wherever it appears in an expression.
    fn expr(&mut self, expr: &Expr, depth: usize) {
        match expr {
            Expr::Closure(def) => {
                if let ClosureBody::Block(body) = &def.body {
                    self.fill(def.pos.line + 1, def.end_pos.line.saturating_sub(1), depth + 1);
                    self.statements(body, depth + 1);
                    self.set(def.end_pos.line, depth);
                }
                if let ClosureBody::Expr(inner) = &def.body {
                    self.expr(inner, depth);
                }
            }
            Expr::List { items, .. } => {
                for item in items {
                    self.expr(item, depth);
                }
            }
            Expr::Dict { entries, .. } => {
                for (_, value) in entries {
                    self.expr(value, depth);
                }
            }
            Expr::Key { obj, .. } => self.expr(obj, depth),
            Expr::Index { obj, index, .. } => {
                self.expr(obj, depth);
                self.expr(index, depth);
            }
            Expr::Call { callee, args, .. } => {
                self.expr(callee, depth);
                for arg in args {
                    self.expr(&arg.value, depth);
                }
            }
            Expr::Unary { operand, .. } => self.expr(operand, depth),
            Expr::Ref { target, .. } => self.expr(target, depth),
            Expr::Binary { left, right, .. } => {
                self.expr(left, depth);
                self.expr(right, depth);
            }
            _ => {}
        }
    }

    /// §12 rule 5: pad every cell to the widest in its column, join with
    /// ` || `, and emit a separator for every column on every row.
    fn pad_rows(&mut self, rows: &[Row]) {
        let mut rendered: Vec<(u32, Vec<String>)> = Vec::new();
        let mut widths: Vec<usize> = Vec::new();
        for row in rows {
            let cells: Vec<String> = row.cells.iter().map(|c| render(&c.tokens)).collect();
            for (i, cell) in cells.iter().enumerate() {
                let width = cell.chars().count();
                match widths.get_mut(i) {
                    Some(w) => *w = (*w).max(width),
                    None => widths.push(width),
                }
            }
            rendered.push((row.line, cells));
        }
        for (line, cells) in rendered {
            let padded: Vec<String> = cells
                .iter()
                .enumerate()
                .map(|(i, cell)| {
                    let width = widths.get(i).copied().unwrap_or(0);
                    let pad = width.saturating_sub(cell.chars().count());
                    format!("{cell}{}", " ".repeat(pad))
                })
                .collect();
            // Trailing padding on the last column is invisible, and trimming it
            // keeps the file free of trailing whitespace.
            self.rows.insert(line, padded.join(" || ").trim_end().to_string());
        }
    }
}

// --- token-level rendering --------------------------------------------------

fn is_operand_end(tok: &Token) -> bool {
    match &tok.kind {
        Tok::Ident(_) | Tok::Num { .. } | Tok::Str { .. } | Tok::Sym { .. } => true,
        Tok::Op(op) => matches!(*op, ")" | "]" | "}"),
        Tok::Kw(kw) => *kw == "end",
        _ => false,
    }
}

/// Which `*` closes a parameter list rather than multiplying: `values*` and the
/// bare `*` take no space before them (channels §6.1). A binary `*` always has
/// an operand after it, so a `*` that a `,` or a `)` follows can only be the
/// other kind.
fn mark_variadic_stars(tokens: &[Token]) -> Vec<bool> {
    let mut variadic = vec![false; tokens.len()];
    for (i, tok) in tokens.iter().enumerate() {
        if tok.is_op("*") {
            let closes = matches!(tokens.get(i + 1), Some(next) if next.is_op(",") || next.is_op(")"));
            // Only `values*` closes up against its name; a bare `*` is a
            // parameter of its own and is spaced like one.
            let named = i > 0 && tokens[i - 1].ident().is_some();
            variadic[i] = closes && named;
        }
    }
    variadic
}

/// Which `-`, `~` and `&` are prefixes rather than binary operators. A prefix
/// takes no space after it — `&a`, never `& a` (rule 2).
fn mark_prefixes(tokens: &[Token]) -> Vec<bool> {
    let mut prefix = vec![false; tokens.len()];
    for (i, tok) in tokens.iter().enumerate() {
        if let Tok::Op(op) = &tok.kind {
            if matches!(*op, "-" | "~" | "&") {
                let after_operand = i > 0 && is_operand_end(&tokens[i - 1]);
                prefix[i] = !after_operand;
            }
        }
    }
    prefix
}

fn render(tokens: &[Token]) -> String {
    let prefix = mark_prefixes(tokens);
    let variadic = mark_variadic_stars(tokens);
    let mut out = String::new();
    for (i, tok) in tokens.iter().enumerate() {
        if i > 0 && !variadic[i] && needs_space(&tokens[i - 1], tok, prefix[i - 1]) {
            out.push(' ');
        }
        out.push_str(&text_of(tok, i.checked_sub(1).map(|j| &tokens[j]), tokens.get(i + 1)));
    }
    out
}

/// Render a token, normalising the two things a token's own `text()` cannot:
/// the expressions inside its interpolations (rule 3a), and a key written as a
/// quoted string — `headers."name"` — which the lexer produced as a lookup dot
/// followed by a string rather than as one symbol token.
fn text_of(tok: &Token, prev: Option<&Token>, next: Option<&Token>) -> String {
    match &tok.kind {
        Tok::Str { parts } => {
            if let (Some(prev), Some(literal)) = (prev, static_text(parts)) {
                // A *key lookup* keeps its quotes unless the key is a plain
                // identifier: `d.total-1` subtracts, so a hyphenated key can
                // only be written `d."x-req-id"` or `d[:x-req-id]`.
                if prev.is_op(".") && is_identifier(&literal) {
                    return literal;
                }
            }
            format!("\"{}\"", render_parts(parts))
        }
        // A bare atom would swallow a following lookup dot: `:"a".key`
        // must not become the single atom `:a.key`.
        Tok::Sym { parts, .. } if next.is_some_and(|t| t.is_op(".")) => {
            format!(":\"{}\"", render_parts(parts))
        }
        Tok::Sym { parts, quoted } => match static_text(parts) {
            // Rule 3a: a quoted symbol whose content is a valid bare atom is
            // rewritten bare. One that interpolates never can be.
            Some(name) if !*quoted || is_symbol_name(&name) => format!(":{name}"),
            _ => format!(":\"{}\"", render_parts(parts)),
        },
        _ => tok.text(),
    }
}

/// The inside of a string: literal runs as written, and interpolations
/// normalised like any other expression — `\(a + b)`, never `\( a+b )` (§12
/// rule 3a).
fn render_parts(parts: &[StrPiece]) -> String {
    let mut out = String::new();
    for part in parts {
        match part {
            StrPiece::Text { raw, .. } => out.push_str(raw),
            StrPiece::Expr { tokens, .. } => {
                let inner: Vec<Token> =
                    tokens.iter().filter(|t| !t.is_eof() && !t.is_newline()).cloned().collect();
                out.push_str("\\(");
                out.push_str(&render(&inner));
                out.push(')');
            }
        }
    }
    out
}

fn needs_space(prev: &Token, cur: &Token, prev_is_prefix: bool) -> bool {
    let prev_op = match &prev.kind {
        Tok::Op(op) => Some(*op),
        _ => None,
    };
    let cur_op = match &cur.kind {
        Tok::Op(op) => Some(*op),
        _ => None,
    };

    // An empty dict stays `{}`; a non-empty one gets spaces inside (rule 3a).
    if prev_op == Some("{") && cur_op == Some("}") {
        return false;
    }
    // Nothing inside `(` or `[` (rule 2).
    if matches!(prev_op, Some("(") | Some("[")) {
        return false;
    }
    if matches!(cur_op, Some(")") | Some("]")) {
        return false;
    }
    if cur_op == Some(",") {
        return false;
    }
    // A key lookup and a namespace selector bind tightest and stay tight.
    if matches!(prev_op, Some(".") | Some("::")) || matches!(cur_op, Some(".") | Some("::")) {
        return false;
    }
    // `&a`, `-x`, `~mask`.
    if prev_is_prefix {
        return false;
    }
    // `f(x)` and `fn(a)` versus `if (a)` and `while (x)`.
    if cur_op == Some("(") {
        return !is_operand_end(prev) && !prev.is_kw("fn");
    }
    if cur_op == Some("[") {
        return !is_operand_end(prev);
    }
    true
}
