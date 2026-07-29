//! The parser (spec §3) and the parallel-block transposer (spec §4).
//!
//! Ordinary statements are one line each, so the parser is a straightforward
//! precedence climber. The unusual part is `parallel` / `race`: those blocks are
//! parsed **column-wise**, by splitting every row on top-level `||` and
//! concatenating cell *k* of every row into trail *k*'s token stream. Every
//! token keeps its original position, so a diagnostic from inside a trail
//! points at the real source line and not at the transposed one (§4 step 6).

use std::sync::Arc;

use crate::ast::*;
use crate::errors::{HydraError, Pos, Result};
use crate::lexer::{
    compound_assign, static_text, tokenize, StrPiece, Tok, Token, TRAIL_LABEL, TRAIL_SEP,
};

/// Keywords that end a block body without being part of it.
const BLOCK_ENDERS: &[&str] = &["end", "else", "else if"];

pub fn parse(src: &str, file: &str) -> Result<Program> {
    let lexed = tokenize(src, file)?;
    let mut parser = Parser::new(lexed.tokens, file, false);
    let body = parser.parse_program()?;
    Ok(Program { body, file: file.to_string(), comments: lexed.comments, line_count: lexed.line_count })
}

pub struct Parser<'a> {
    toks: Vec<Token>,
    i: usize,
    file: &'a str,
    /// True while parsing a transposed trail stream. A `parallel` or `race`
    /// block may not appear syntactically inside a cell (§4).
    in_cell: bool,
}

impl<'a> Parser<'a> {
    pub fn new(toks: Vec<Token>, file: &'a str, in_cell: bool) -> Parser<'a> {
        Parser { toks, i: 0, file, in_cell }
    }

    // --- token access -------------------------------------------------------

    fn peek(&self) -> &Token {
        &self.toks[self.i.min(self.toks.len() - 1)]
    }

    fn peek_at(&self, ahead: usize) -> &Token {
        &self.toks[(self.i + ahead).min(self.toks.len() - 1)]
    }

    fn pos(&self) -> Pos {
        self.peek().pos
    }

    fn advance(&mut self) -> Token {
        let tok = self.peek().clone();
        if self.i < self.toks.len() - 1 {
            self.i += 1;
        }
        tok
    }

    fn err<T>(&self, message: impl Into<String>, pos: Pos) -> Result<T> {
        Err(HydraError::new(message, self.file, pos))
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if self.peek().is_op(op) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.peek().is_kw(kw) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_op(&mut self, op: &str) -> Result<Token> {
        if self.peek().is_op(op) {
            Ok(self.advance())
        } else {
            self.err(format!("expected `{op}`, found {}", self.peek()), self.pos())
        }
    }

    fn expect_kw(&mut self, kw: &str) -> Result<Token> {
        if self.peek().is_kw(kw) {
            Ok(self.advance())
        } else {
            self.err(format!("expected `{kw}`, found {}", self.peek()), self.pos())
        }
    }

    fn expect_ident(&mut self, what: &str) -> Result<(String, Pos)> {
        let pos = self.pos();
        match self.peek().ident() {
            Some(name) => {
                let name = name.to_string();
                self.advance();
                Ok((name, pos))
            }
            None => self.err(format!("expected {what}, found {}", self.peek()), pos),
        }
    }

    /// A newline terminates a statement; end of file does too (§1).
    fn expect_end_of_statement(&mut self) -> Result<()> {
        if self.peek().is_newline() {
            self.advance();
            return Ok(());
        }
        if self.peek().is_eof() {
            return Ok(());
        }
        self.err(format!("expected end of line, found {}", self.peek()), self.pos())
    }

    fn skip_newlines(&mut self) {
        while self.peek().is_newline() {
            self.advance();
        }
    }

    // --- program and blocks -------------------------------------------------

    pub fn parse_program(&mut self) -> Result<Vec<Stmt>> {
        let mut body = Vec::new();
        loop {
            self.skip_newlines();
            if self.peek().is_eof() {
                break;
            }
            body.push(self.parse_stmt()?);
        }
        Ok(body)
    }

    /// Parse statements until one of `BLOCK_ENDERS`, which is left unconsumed.
    fn parse_block(&mut self) -> Result<Vec<Stmt>> {
        let mut body = Vec::new();
        loop {
            self.skip_newlines();
            if self.peek().is_any_kw(BLOCK_ENDERS) {
                break;
            }
            if self.peek().is_eof() {
                return self.err("expected `end`, found end of file", self.pos());
            }
            body.push(self.parse_stmt()?);
        }
        Ok(body)
    }

    fn expect_block_end(&mut self) -> Result<Pos> {
        let pos = self.pos();
        self.expect_kw("end")?;
        self.expect_end_of_statement()?;
        Ok(pos)
    }

    /// An optional `as name` label on a loop or block (§9.6).
    fn parse_label(&mut self) -> Result<Option<String>> {
        if !self.eat_kw("as") {
            return Ok(None);
        }
        let (name, pos) = self.expect_ident("a label name")?;
        if name == TRAIL_LABEL {
            return self.err("`trail` is a reserved label and cannot be declared", pos);
        }
        Ok(Some(name))
    }

    // --- statements ---------------------------------------------------------

    fn parse_stmt(&mut self) -> Result<Stmt> {
        let tok = self.peek().clone();
        let pos = tok.pos;

        if let Tok::Kw(kw) = tok.kind {
            match kw {
                "use" => return self.parse_use(pos),
                "fn" if self.peek_at(1).ident().is_some() => return self.parse_fn_decl(pos),
                "if" => return self.parse_if(pos),
                "for" => return self.parse_for(pos),
                "while" => return self.parse_while(pos),
                "parallel" | "race" => return self.parse_parallel_rows(kw, pos),
                "parallel for" | "race for" => return self.parse_parallel_for(kw, pos),
                "parallel while" | "race while" => return self.parse_parallel_while(kw, pos),
                "break" => return self.parse_break(pos),
                "continue" => return self.parse_continue(pos),
                "return" => return self.parse_return(pos),
                "end" => return self.err("`end` without a matching block", pos),
                _ => {}
            }
        }

        // Everything else starts with an expression: it is a declaration, an
        // assignment, or an expression statement.
        let expr = self.parse_expr()?;

        if self.peek().is_op(":=") {
            let op_pos = self.pos();
            self.advance();
            let value = self.parse_expr()?;
            self.expect_end_of_statement()?;
            return match expr {
                Expr::Name { name, .. } => Ok(Stmt::Decl { name, value, pos }),
                _ => self.err("the left of `:=` must be a name", op_pos),
            };
        }

        // `=`, and the compound assignments that carry an operator with them.
        // They take the same target and the same rule about what a target may
        // be, so they are one branch.
        if let Tok::Op(spelling) = self.peek().kind {
            let op = if spelling == "=" { Some(None) } else { compound_assign(spelling).map(Some) };
            if let Some(op) = op {
                let op_pos = self.pos();
                self.advance();
                let value = self.parse_expr()?;
                self.expect_end_of_statement()?;
                if !expr.is_lvalue() {
                    return self.err(
                        format!(
                            "the left of `{spelling}` must be a variable, a dict key or a list element"
                        ),
                        op_pos,
                    );
                }
                return Ok(Stmt::Assign { target: expr, op, value, pos });
            }
        }

        self.expect_end_of_statement()?;
        Ok(Stmt::Expr { expr, pos })
    }

    fn parse_use(&mut self, pos: Pos) -> Result<Stmt> {
        self.advance();
        let (module, _) = self.expect_ident("a module name")?;
        self.expect_end_of_statement()?;
        Ok(Stmt::Use { module, pos })
    }

    fn parse_fn_decl(&mut self, pos: Pos) -> Result<Stmt> {
        self.advance(); // fn
        let (name, _) = self.expect_ident("a function name")?;
        let params = self.parse_params()?;
        if !self.peek().is_newline() {
            return self.err(
                "a function declaration's body starts on the next line; \
                 write `x := fn(…) expr` for a single-expression closure",
                self.pos(),
            );
        }
        self.advance();
        let body = self.parse_block()?;
        let end_pos = self.expect_block_end()?;
        let def = Arc::new(ClosureDef {
            name: name.clone(),
            params,
            body: ClosureBody::Block(body),
            pos,
            end_pos,
        });
        Ok(Stmt::FnDecl { name, def, pos })
    }

    /// `param = [ "&" ] ident [ ":=" expr ]`.
    fn parse_params(&mut self) -> Result<Vec<Param>> {
        self.expect_op("(")?;
        let mut params: Vec<Param> = Vec::new();
        if !self.peek().is_op(")") {
            loop {
                let by_ref = self.eat_op("&");
                if matches!(self.peek().kind, Tok::Kw(_)) {
                    return self.err(
                        format!(
                            "`{}` is a keyword and cannot be a parameter name",
                            self.peek().text()
                        ),
                        self.pos(),
                    );
                }
                let (name, pos) = self.expect_ident("a parameter name")?;
                if params.iter().any(|p| p.name == name) {
                    return self.err(format!("duplicate parameter `{name}`"), pos);
                }
                let default = if self.eat_op("=") { Some(self.parse_expr()?) } else { None };

                if by_ref && default.is_some() {
                    return self.err(
                        format!(
                            "`&{name}` cannot have a default: a default is a value,                              and a reference has to come from a call site"
                        ),
                        pos,
                    );
                }
                // Positional filling only, so a defaulted parameter cannot be
                // followed by one that must be supplied.
                if default.is_none() && params.iter().any(|p| p.default.is_some()) {
                    return self.err(
                        format!("`{name}` has no default but follows one that does"),
                        pos,
                    );
                }
                params.push(Param { name, by_ref, default, pos });
                if !self.eat_op(",") {
                    break;
                }
            }
        }
        self.expect_op(")")?;
        Ok(params)
    }

    fn parse_if(&mut self, pos: Pos) -> Result<Stmt> {
        let mut branches = Vec::new();
        let mut clause_pos = pos;
        self.advance(); // if
        let cond = self.parse_expr()?;
        self.expect_end_of_statement()?;
        branches.push(Branch { cond: Some(cond), body: self.parse_block()?, pos: clause_pos, keyword: "if" });

        loop {
            clause_pos = self.pos();
            if self.eat_kw("else if") {
                let cond = self.parse_expr()?;
                self.expect_end_of_statement()?;
                branches.push(Branch {
                    cond: Some(cond),
                    body: self.parse_block()?,
                    pos: clause_pos,
                    keyword: "else if",
                });
                continue;
            }
            if self.eat_kw("else") {
                self.expect_end_of_statement()?;
                branches.push(Branch {
                    cond: None,
                    body: self.parse_block()?,
                    pos: clause_pos,
                    keyword: "else",
                });
            }
            break;
        }

        let end_pos = self.expect_block_end()?;
        Ok(Stmt::If { branches, pos, end_pos })
    }

    fn parse_for(&mut self, pos: Pos) -> Result<Stmt> {
        self.advance();
        let (var, _) = self.expect_ident("a loop variable")?;
        self.expect_kw("in")?;
        let iterable = self.parse_expr()?;
        let label = self.parse_label()?;
        self.expect_end_of_statement()?;
        let body = self.parse_block()?;
        let end_pos = self.expect_block_end()?;
        Ok(Stmt::For { var, iterable, body, label, pos, end_pos })
    }

    fn parse_while(&mut self, pos: Pos) -> Result<Stmt> {
        self.advance();
        let cond = self.parse_expr()?;
        let label = self.parse_label()?;
        self.expect_end_of_statement()?;
        let body = self.parse_block()?;
        let end_pos = self.expect_block_end()?;
        Ok(Stmt::While { cond, body, label, pos, end_pos })
    }

    fn parse_break(&mut self, pos: Pos) -> Result<Stmt> {
        self.advance();
        let target = match self.peek().ident() {
            Some(name) if name == TRAIL_LABEL => {
                self.advance();
                BreakTarget::Trail
            }
            Some(name) => {
                let name = name.to_string();
                self.advance();
                BreakTarget::Label(name)
            }
            None => BreakTarget::Innermost,
        };
        self.expect_end_of_statement()?;
        Ok(Stmt::Break { target, pos })
    }

    fn parse_continue(&mut self, pos: Pos) -> Result<Stmt> {
        self.advance();
        let label = match self.peek().ident() {
            Some(name) if name == TRAIL_LABEL => {
                return self.err("`continue trail` does not exist; only `break trail` does", self.pos())
            }
            Some(name) => {
                let name = name.to_string();
                self.advance();
                Some(name)
            }
            None => None,
        };
        self.expect_end_of_statement()?;
        Ok(Stmt::Continue { label, pos })
    }

    fn parse_return(&mut self, pos: Pos) -> Result<Stmt> {
        self.advance();
        let value = if self.peek().is_newline() || self.peek().is_eof() {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.expect_end_of_statement()?;
        Ok(Stmt::Return { value, pos })
    }

    // --- parallel blocks (§4) ----------------------------------------------

    fn block_kind(kw: &str) -> BlockKind {
        if kw.starts_with("race") {
            BlockKind::Race
        } else {
            BlockKind::Parallel
        }
    }

    fn reject_nested_block(&self, kw: &str, pos: Pos) -> Result<()> {
        if self.in_cell {
            return self.err(
                format!(
                    "a `{kw}` block may not be written inside a cell; \
                     call a function that opens the inner block instead"
                ),
                pos,
            );
        }
        Ok(())
    }

    fn parse_parallel_for(&mut self, kw: &'static str, pos: Pos) -> Result<Stmt> {
        self.reject_nested_block(kw, pos)?;
        self.advance();
        let (var, _) = self.expect_ident("a loop variable")?;
        self.expect_kw("in")?;
        let iterable = self.parse_expr()?;
        let label = self.parse_label()?;
        self.expect_end_of_statement()?;
        let body = self.parse_block()?;
        let end_pos = self.expect_block_end()?;
        Ok(Stmt::ParallelFor { kind: Self::block_kind(kw), var, iterable, body, label, pos, end_pos })
    }

    fn parse_parallel_while(&mut self, kw: &'static str, pos: Pos) -> Result<Stmt> {
        self.reject_nested_block(kw, pos)?;
        self.advance();
        let cond = self.parse_expr()?;
        let label = self.parse_label()?;
        self.expect_end_of_statement()?;
        let body = self.parse_block()?;
        let end_pos = self.expect_block_end()?;
        Ok(Stmt::ParallelWhile { kind: Self::block_kind(kw), cond, body, label, pos, end_pos })
    }

    /// The row form. Read raw lines until a line with zero separators whose
    /// only token is `end`, split every row on top-level `||`, check that the
    /// rows agree, then transpose (§4).
    fn parse_parallel_rows(&mut self, kw: &'static str, pos: Pos) -> Result<Stmt> {
        self.reject_nested_block(kw, pos)?;
        self.advance();
        let label = self.parse_label()?;
        self.expect_end_of_statement()?;

        let mut rows: Vec<Row> = Vec::new();
        let end_pos;
        loop {
            if self.peek().is_eof() {
                return self.err(format!("expected `end` to close this `{kw}` block"), pos);
            }
            let line_pos = self.pos();
            let line = self.take_line();

            // A line with no tokens at all is not a row: blank lines and
            // comment-only lines stay out of the separator count.
            if line.is_empty() {
                continue;
            }

            let seps = count_top_level_separators(&line);
            if seps == 0 && line.len() == 1 && line[0].is_kw("end") {
                end_pos = line[0].pos;
                break;
            }
            rows.push(Row { cells: self.split_row(&line)?, line: line_pos.line });
        }

        // `parallel` on one line and `for` on the next is a compound keyword
        // split across lines (§2). It would otherwise parse as a block whose
        // single trail holds a loop, and then fail somewhere much less useful.
        if let Some(first) = rows.first() {
            if first.cells.len() == 1 {
                if let Some(token) = first.cells[0].tokens.first() {
                    if let Tok::Kw(tail @ ("for" | "while")) = token.kind {
                        return self.err(
                            format!(
                                "`{kw} {tail}` is one compound keyword and may not be split across \
                                 lines; write it on the block's own line"
                            ),
                            token.pos,
                        );
                    }
                }
            }
        }

        let width = rows.first().map(|r| r.cells.len()).unwrap_or(1);
        for row in &rows {
            if row.cells.len() != width {
                let site = row.cells.last().map(|c| c.pos).unwrap_or(pos);
                return self.err(
                    format!(
                        "every row of a `{kw}` block carries the same number of `||`: \
                         this row has {} cell(s), the first row has {width}",
                        row.cells.len()
                    ),
                    site,
                );
            }
        }

        // Transpose: cell k of every row, in row order, is trail k (§4 step 4).
        let mut trails = Vec::new();
        for column in 0..width {
            let mut stream: Vec<Token> = Vec::new();
            let mut first_pos: Option<Pos> = None;
            for row in &rows {
                let cell = &row.cells[column];
                if cell.tokens.is_empty() {
                    continue;
                }
                first_pos.get_or_insert(cell.pos);
                let last = cell.tokens[cell.tokens.len() - 1].pos;
                stream.extend(cell.tokens.iter().cloned());
                stream.push(Token::new(Tok::Newline, last));
            }
            stream.push(Token::new(Tok::Eof, self.pos()));
            let mut sub = Parser::new(stream, self.file, true);
            let body = sub.parse_program()?;
            trails.push(TrailDef { body, column, pos: first_pos.unwrap_or(pos) });
        }

        Ok(Stmt::Parallel { kind: Self::block_kind(kw), trails, rows, label, pos, end_pos })
    }

    /// Take one physical line's tokens, consuming the newline.
    fn take_line(&mut self) -> Vec<Token> {
        let mut line = Vec::new();
        while !self.peek().is_newline() && !self.peek().is_eof() {
            line.push(self.advance());
        }
        if self.peek().is_newline() {
            self.advance();
        }
        line
    }

    fn split_row(&self, line: &[Token]) -> Result<Vec<Cell>> {
        let mut cells: Vec<Cell> = Vec::new();
        let mut current: Vec<Token> = Vec::new();
        let mut depth = 0i32;
        let mut cell_pos = line.first().map(|t| t.pos).unwrap_or(Pos::NONE);

        for tok in line {
            if let Tok::Op(op) = tok.kind {
                match op {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => depth -= 1,
                    TRAIL_SEP if depth == 0 => {
                        cells.push(Cell { tokens: std::mem::take(&mut current), pos: cell_pos });
                        cell_pos = tok.pos;
                        continue;
                    }
                    _ => {}
                }
            }
            if current.is_empty() {
                cell_pos = tok.pos;
            }
            current.push(tok.clone());
        }
        cells.push(Cell { tokens: current, pos: cell_pos });

        // A compound keyword may not be split across a cell boundary (§2).
        for pair in cells.windows(2) {
            let (left, right) = (&pair[0], &pair[1]);
            if let (Some(last), Some(first)) = (left.tokens.last(), right.tokens.first()) {
                if let (Tok::Kw(head), Tok::Kw(tail)) = (&last.kind, &first.kind) {
                    if crate::lexer::COMPOUND.iter().any(|(h, t, _)| h == head && t == tail) {
                        return self.err(
                            format!("the compound keyword `{head} {tail}` may not be split across a `||`"),
                            last.pos,
                        );
                    }
                }
            }
        }

        Ok(cells)
    }

    // --- string interpolation (§1) -----------------------------------------

    /// Parse the pieces of a string or quoted symbol. Each `\(…)` was lexed
    /// recursively, so each one is parsed here as an ordinary expression.
    fn str_parts(&self, pieces: &[StrPiece]) -> Result<Vec<StrPart>> {
        let mut parts = Vec::new();
        for piece in pieces {
            match piece {
                StrPiece::Text { value, .. } => {
                    if !value.is_empty() {
                        parts.push(StrPart::Text(value.clone()));
                    }
                }
                StrPiece::Expr { tokens, pos } => {
                    let mut sub = Parser::new(tokens.clone(), self.file, self.in_cell);
                    let expr = sub.parse_expr()?;
                    if !sub.peek().is_eof() {
                        return self.err(
                            format!("unexpected {} in an interpolation", sub.peek()),
                            sub.pos(),
                        );
                    }
                    let _ = pos;
                    parts.push(StrPart::Expr(expr));
                }
            }
        }
        Ok(parts)
    }

    fn sym_lit(&self, pieces: &[StrPiece], quoted: bool, pos: Pos) -> Result<SymLit> {
        let parts = self.str_parts(pieces)?;
        let name = static_text(pieces).unwrap_or_default();
        Ok(SymLit { name, quoted, parts, pos })
    }

    // --- expressions (§3 precedence table) ---------------------------------

    pub fn parse_expr(&mut self) -> Result<Expr> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr> {
        let mut left = self.parse_and()?;
        while self.peek().is_kw("or") {
            let pos = self.advance().pos;
            let right = self.parse_and()?;
            left = Expr::Binary { op: "or", left: Box::new(left), right: Box::new(right), pos };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr> {
        let mut left = self.parse_not()?;
        while self.peek().is_kw("and") {
            let pos = self.advance().pos;
            let right = self.parse_not()?;
            left = Expr::Binary { op: "and", left: Box::new(left), right: Box::new(right), pos };
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr> {
        if self.peek().is_kw("not") {
            let pos = self.advance().pos;
            let operand = self.parse_not()?;
            return Ok(Expr::Unary { op: "not", operand: Box::new(operand), pos });
        }
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<Expr> {
        const OPS: &[&str] = &["==", "!=", "===", "!==", "<", ">", "<=", ">="];
        self.parse_left_assoc(OPS, Parser::parse_bit_or)
    }

    fn parse_bit_or(&mut self) -> Result<Expr> {
        self.parse_left_assoc(&["|"], Parser::parse_bit_xor)
    }

    fn parse_bit_xor(&mut self) -> Result<Expr> {
        self.parse_left_assoc(&["^"], Parser::parse_bit_and)
    }

    fn parse_bit_and(&mut self) -> Result<Expr> {
        self.parse_left_assoc(&["&"], Parser::parse_shift)
    }

    fn parse_shift(&mut self) -> Result<Expr> {
        self.parse_left_assoc(&["<<", ">>", ">>>"], Parser::parse_additive)
    }

    fn parse_additive(&mut self) -> Result<Expr> {
        self.parse_left_assoc(&["+", "-"], Parser::parse_multiplicative)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr> {
        self.parse_left_assoc(&["*", "/", "%"], Parser::parse_unary)
    }

    fn parse_left_assoc(
        &mut self,
        ops: &[&str],
        next: fn(&mut Parser<'a>) -> Result<Expr>,
    ) -> Result<Expr> {
        let mut left = next(self)?;
        while let Tok::Op(op) = self.peek().kind {
            if !ops.contains(&op) {
                break;
            }
            let pos = self.advance().pos;
            let right = next(self)?;
            left = Expr::Binary { op, left: Box::new(left), right: Box::new(right), pos };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        let pos = self.pos();
        if self.peek().is_op("-") {
            self.advance();
            let operand = self.parse_unary()?;
            return Ok(Expr::Unary { op: "-", operand: Box::new(operand), pos });
        }
        if self.peek().is_op("~") {
            self.advance();
            let operand = self.parse_unary()?;
            return Ok(Expr::Unary { op: "~", operand: Box::new(operand), pos });
        }
        if self.peek().is_op("&") {
            self.advance();
            let target = self.parse_unary()?;
            // §5.1: only an lvalue may be referenced. `check` reports this too,
            // but the message is more useful with the token in hand.
            if !target.is_lvalue() {
                return self.err(
                    "`&` takes a variable, a dict key or a list element",
                    target.pos(),
                );
            }
            return Ok(Expr::Ref { target: Box::new(target), pos });
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
            let pos = self.pos();
            if self.peek().is_op("(") {
                self.advance();
                let args = self.parse_args()?;
                self.expect_op(")")?;
                expr = Expr::Call { callee: Box::new(expr), args, pos };
                continue;
            }
            if self.peek().is_op("[") {
                self.advance();
                let index = self.parse_expr()?;
                self.expect_op("]")?;
                expr = Expr::Index { obj: Box::new(expr), index: Box::new(index), pos };
                continue;
            }
            if self.peek().is_op(".") {
                self.advance();
                let key = match &self.peek().kind {
                    Tok::Ident(name) => SymLit::plain(name.clone(), false, pos),
                    // `headers."content-type"` is `headers[."content-type"]`,
                    // and it may interpolate like any other quoted symbol (§2).
                    Tok::Str { parts } => self.sym_lit(parts, true, pos)?,
                    _ => return self.err(
                        format!("expected a key name after `.`, found {}", self.peek()),
                        self.pos(),
                    ),
                };
                self.advance();
                expr = Expr::Key { obj: Box::new(expr), key, pos };
                continue;
            }
            if self.peek().is_op("::") {
                self.advance();
                let (name, _) = self.expect_ident("a name after `::`")?;
                let Expr::Name { name: module, .. } = expr else {
                    return self.err("`::` selects from a module name, as in `json::decode`", pos);
                };
                expr = Expr::Namespace { module, name, pos };
                continue;
            }
            break;
        }
        Ok(expr)
    }

    /// `arg = [ ident "=" ] expr`. A named argument fills the parameter it
    /// names; they come after the positional ones.
    fn parse_args(&mut self) -> Result<Vec<Arg>> {
        let mut args: Vec<Arg> = Vec::new();
        if self.peek().is_op(")") {
            return Ok(args);
        }
        loop {
            let pos = self.pos();
            // `name = value` is a named argument. There is no ambiguity with an
            // assignment: assignment is a statement, never an expression (§3).
            // A keyword cannot be a name, so say that rather than "expected an
            // expression" when someone writes `f(end = 1)`.
            if matches!(self.peek().kind, Tok::Kw(_)) && self.peek_at(1).is_op("=") {
                return self.err(
                    format!(
                        "`{}` is a keyword and cannot be an argument name",
                        self.peek().text()
                    ),
                    pos,
                );
            }
            let named = self.peek().ident().is_some() && self.peek_at(1).is_op("=");
            if named {
                let (name, _) = self.expect_ident("an argument name")?;
                self.expect_op("=")?;
                let value = self.parse_expr()?;
                if args.iter().any(|a| a.name.as_deref() == Some(name.as_str())) {
                    return self.err(format!("`{name}` is given twice in this call"), pos);
                }
                args.push(Arg { name: Some(name), value, pos });
            } else {
                if args.iter().any(|a| a.name.is_some()) {
                    return self.err(
                        "a positional argument cannot follow a named one",
                        pos,
                    );
                }
                args.push(Arg { name: None, value: self.parse_expr()?, pos });
            }
            if !self.eat_op(",") {
                break;
            }
        }
        Ok(args)
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        let tok = self.peek().clone();
        let pos = tok.pos;
        match tok.kind {
            Tok::Num { value, raw } => {
                self.advance();
                Ok(Expr::Num { value, raw, pos })
            }
            Tok::Str { parts } => {
                let parts = self.str_parts(&parts)?;
                self.advance();
                Ok(Expr::Str { parts, pos })
            }
            Tok::Sym { parts, quoted } => {
                let sym = self.sym_lit(&parts, quoted, pos)?;
                self.advance();
                Ok(Expr::Sym(sym))
            }
            Tok::Ident(name) => {
                self.advance();
                Ok(Expr::Name { name, pos })
            }
            Tok::Kw("fn") => self.parse_closure(pos),
            Tok::Op("(") => {
                self.advance();
                let inner = self.parse_expr()?;
                self.expect_op(")")?;
                Ok(inner)
            }
            Tok::Op("[") => {
                self.advance();
                let mut items = Vec::new();
                if !self.peek().is_op("]") {
                    loop {
                        items.push(self.parse_expr()?);
                        if !self.eat_op(",") {
                            break;
                        }
                    }
                }
                self.expect_op("]")?;
                Ok(Expr::List { items, pos })
            }
            Tok::Op("{") => self.parse_dict(pos),
            // `::name` selects the language's own namespace: the builtin, even
            // where something else has taken the name (§7).
            Tok::Op("::") => {
                self.advance();
                let (name, _) = self.expect_ident("a builtin name after `::`")?;
                Ok(Expr::Namespace { module: String::new(), name, pos })
            }
            _ => self.err(format!("expected an expression, found {tok}"), pos),
        }
    }

    fn parse_dict(&mut self, pos: Pos) -> Result<Expr> {
        self.advance(); // {
        let mut entries: Vec<(SymLit, Expr)> = Vec::new();
        if !self.peek().is_op("}") {
            loop {
                let key_pos = self.pos();
                let key = match &self.peek().kind {
                    Tok::Sym { parts, quoted } => self.sym_lit(parts, *quoted, key_pos)?,
                    _ => {
                        return self.err(
                            format!(
                                "a dict key must be a symbol such as `.name` or `.\"x-req-id\"`, found {}",
                                self.peek()
                            ),
                            key_pos,
                        )
                    }
                };
                self.advance();
                self.expect_op(":")?;
                let value = self.parse_expr()?;
                entries.push((key, value));
                if !self.eat_op(",") {
                    break;
                }
            }
        }
        self.expect_op("}")?;
        Ok(Expr::Dict { entries, pos })
    }

    /// `fn (params) expr` or `fn (params) NEWLINE block end`. Which one it is
    /// depends on whether anything follows the `)` on the same line (§3).
    fn parse_closure(&mut self, pos: Pos) -> Result<Expr> {
        self.advance(); // fn
        let params = self.parse_params()?;
        if self.peek().is_newline() {
            self.advance();
            let body = self.parse_block()?;
            let end_pos = self.pos();
            self.expect_kw("end")?;
            return Ok(Expr::Closure(Arc::new(ClosureDef {
                name: String::new(),
                params,
                body: ClosureBody::Block(body),
                pos,
                end_pos,
            })));
        }
        let expr = self.parse_expr()?;
        Ok(Expr::Closure(Arc::new(ClosureDef {
            name: String::new(),
            params,
            body: ClosureBody::Expr(Box::new(expr)),
            pos,
            end_pos: Pos::NONE,
        })))
    }
}

/// Count `||` separators that are not inside `(`, `[`, `{` or a string.
///
/// Strings are single tokens by the time this runs, which is exactly why §4
/// insists this step works on tokens.
pub fn count_top_level_separators(line: &[Token]) -> usize {
    let mut depth = 0i32;
    let mut count = 0;
    for tok in line {
        if let Tok::Op(op) = tok.kind {
            match op {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth -= 1,
                TRAIL_SEP if depth == 0 => count += 1,
                _ => {}
            }
        }
    }
    count
}

/// Split a token stream into physical lines, dropping the newline tokens.
pub fn split_lines(tokens: &[Token]) -> Vec<Vec<Token>> {
    let mut lines: Vec<Vec<Token>> = Vec::new();
    let mut current: Vec<Token> = Vec::new();
    for tok in tokens {
        match tok.kind {
            Tok::Newline => lines.push(std::mem::take(&mut current)),
            Tok::Eof => break,
            _ => current.push(tok.clone()),
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

