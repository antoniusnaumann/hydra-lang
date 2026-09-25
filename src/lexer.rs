//! The lexer (spec §1 and §2).
//!
//! Two things here are not the usual thing:
//!
//! * **Compound keywords.** `else if`, `parallel for`, `parallel while`,
//!   `race for` and `race while` lex as *one* token. The internal whitespace is
//!   spaces or tabs, never a newline, so `else` at end of line followed by `if`
//!   on the next line is two tokens and means something different (§2).
//!
//! * **Atoms and lookups are distinct.** `:name` opens an atom; a dot is
//!   always a lookup operator. `:=` and `::` keep their existing meanings.
//!
//! * **Strings are not opaque.** `\(expr)` is lexed *recursively*, so a string
//!   token carries a token stream for each interpolation. That is what §1 means
//!   when it says every tool has to work on tokens: a string can contain `||`,
//!   `end`, `//`, and a whole expression with strings of its own.

use std::collections::HashMap;
use std::fmt;

use crate::errors::{HydraError, Pos, Result};

/// Keywords (§2). Loop control uses ordinary atoms.
pub const KEYWORDS: &[&str] = &[
    "fn", "use", "if", "else", "for", "in", "while", "return", "end", "and",
    "or", "not", "parallel", "race", "as",
];

/// Compound keywords: single keywords that contain a space (§2).
pub const COMPOUND: &[(&str, &str, &str)] = &[
    ("else", "if", "else if"),
    ("parallel", "for", "parallel for"),
    ("parallel", "while", "parallel while"),
    ("race", "for", "race for"),
    ("race", "while", "race while"),
];

/// Longest match wins, and the order matters (§2): `===` before `==` before
/// `=`, `!==` before `!=`, `::` and `:=` before `:`, `||` before `|`, `>>>`
/// before `>>` before `>`, and every compound assignment before the operator it
/// is built from — `>>>=` before `>>>`, `+=` before `+`. The table is written
/// longest-first so that the rule is visible rather than implied.
pub const OPERATORS: &[&str] = &[
    ">>>=", //
    ">>>", "<<=", ">>=", "===", "!==", //
    "==", "!=", "<=", ">=", "<<", ">>", ":=", "::", "||", //
    "+=", "-=", "*=", "/=", "%=", "|=", "&=", "^=", //
    "=", "<", ">", "+", "-", "*", "/", "%", //
    "|", "&", "^", "~", //
    "(", ")", "[", "]", "{", "}", ",", ".", ":",
];

/// The compound assignments, each paired with the binary operator it applies.
///
/// One per arithmetic and bitwise operator of §3's table, and none for the
/// comparisons or for `and` / `or`: `a <= b` and `a and b` answer a question
/// rather than combining two operands into a new one.
pub const COMPOUND_ASSIGN: &[(&str, &str)] = &[
    ("+=", "+"),
    ("-=", "-"),
    ("*=", "*"),
    ("/=", "/"),
    ("%=", "%"),
    ("|=", "|"),
    ("&=", "&"),
    ("^=", "^"),
    ("<<=", "<<"),
    (">>=", ">>"),
    (">>>=", ">>>"),
];

/// The binary operator a compound assignment applies, if it is one.
pub fn compound_assign(op: &str) -> Option<&'static str> {
    COMPOUND_ASSIGN.iter().find(|(spelling, _)| *spelling == op).map(|(_, binary)| *binary)
}

/// The trail separator (§2). Not logical or — Hydra has no `||` operator.
pub const TRAIL_SEP: &str = "||";

/// One piece of a string literal or a quoted symbol.
#[derive(Clone, Debug, PartialEq)]
pub enum StrPiece {
    /// A literal run. `raw` is how it was spelled, so a format pass puts the
    /// escapes back exactly as they were written.
    Text { value: String, raw: String },
    /// A `\(…)` interpolation, lexed recursively (§1).
    Expr { tokens: Vec<Token>, pos: Pos },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Newline,
    Eof,
    Ident(String),
    Num { value: f64, raw: String },
    Str { parts: Vec<StrPiece> },
    Sym { parts: Vec<StrPiece>, quoted: bool },
    Kw(&'static str),
    Op(&'static str),
}

/// The literal text of `parts`, or `None` if any of it is interpolated.
pub fn static_text(parts: &[StrPiece]) -> Option<String> {
    let mut out = String::new();
    for part in parts {
        match part {
            StrPiece::Text { value, .. } => out.push_str(value),
            StrPiece::Expr { .. } => return None,
        }
    }
    Some(out)
}

/// The source spelling of `parts`, between the quotes.
pub fn raw_text(parts: &[StrPiece]) -> String {
    let mut out = String::new();
    for part in parts {
        match part {
            StrPiece::Text { raw, .. } => out.push_str(raw),
            StrPiece::Expr { tokens, .. } => {
                out.push_str("\\(");
                for tok in tokens.iter().filter(|t| !t.is_eof()) {
                    out.push_str(&tok.text());
                }
                out.push(')');
            }
        }
    }
    out
}

pub fn text_piece(value: &str) -> StrPiece {
    StrPiece::Text { value: value.to_string(), raw: escape_string(value) }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: Tok,
    pub pos: Pos,
    /// How many source characters the token spans. Not always the length of
    /// its canonical spelling: `else   if` is one keyword whose text is
    /// `else if`, and editor tooling needs the source extent (§13).
    pub len: u32,
}

impl Token {
    pub fn new(kind: Tok, pos: Pos) -> Token {
        Token { kind, pos, len: 0 }
    }

    pub fn is_op(&self, op: &str) -> bool {
        matches!(&self.kind, Tok::Op(o) if *o == op)
    }

    pub fn is_kw(&self, kw: &str) -> bool {
        matches!(&self.kind, Tok::Kw(k) if *k == kw)
    }

    pub fn is_any_kw(&self, kws: &[&str]) -> bool {
        matches!(&self.kind, Tok::Kw(k) if kws.contains(k))
    }

    pub fn is_newline(&self) -> bool {
        matches!(self.kind, Tok::Newline)
    }

    pub fn is_eof(&self) -> bool {
        matches!(self.kind, Tok::Eof)
    }

    pub fn ident(&self) -> Option<&str> {
        match &self.kind {
            Tok::Ident(name) => Some(name),
            _ => None,
        }
    }

    /// The literal text of a string or symbol token, when it does not
    /// interpolate.
    pub fn static_str(&self) -> Option<String> {
        match &self.kind {
            Tok::Str { parts } | Tok::Sym { parts, .. } => static_text(parts),
            _ => None,
        }
    }

    /// The canonical spelling of this token, which is what the formatter emits.
    pub fn text(&self) -> String {
        match &self.kind {
            Tok::Newline => "\n".into(),
            Tok::Eof => String::new(),
            Tok::Ident(name) => name.clone(),
            Tok::Num { raw, .. } => raw.clone(),
            Tok::Str { parts } => format!("\"{}\"", raw_text(parts)),
            Tok::Sym { parts, quoted } => {
                // §12 rule 3a: a quoted symbol whose content is a valid
                // bare atom is rewritten bare. An interpolated one never is.
                match static_text(parts) {
                    Some(name) if !*quoted || is_symbol_name(&name) => format!(":{name}"),
                    _ => format!(":\"{}\"", raw_text(parts)),
                }
            }
            Tok::Kw(k) => (*k).to_string(),
            Tok::Op(o) => (*o).to_string(),
        }
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            Tok::Newline => write!(f, "end of line"),
            Tok::Eof => write!(f, "end of file"),
            other => write!(f, "`{}`", Token::new(other.clone(), self.pos).text()),
        }
    }
}

pub fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic()
}

pub fn is_ident_char(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}

/// `[A-Za-z_][A-Za-z0-9_]*` (§2).
pub fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) if is_ident_start(c) => chars.all(is_ident_char),
        _ => false,
    }
}

/// Bare atoms start like identifiers and consume everything up to whitespace,
/// a structural delimiter, a comment (`//`), or a trail separator (`||`).
/// Lookup names still use the ordinary identifier rules.
pub fn is_symbol_name(text: &str) -> bool {
    text.chars().next().is_some_and(is_ident_start)
        && text.chars().all(|c| !is_symbol_delimiter(c))
        && !text.contains("//")
        && !text.contains("||")
}

fn is_symbol_delimiter(c: char) -> bool {
    c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | '{' | '}' | ',' | ':' | '"')
}

/// A leading underscore marks an item private (§2).
pub fn is_private(name: &str) -> bool {
    name.starts_with('_')
}

pub fn escape_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            other => out.push(other),
        }
    }
    out
}

/// The result of lexing a file: the tokens, plus the comments the formatter
/// needs to put back. A comment runs to end of line, so there is at most one
/// per line.
pub struct Lexed {
    pub tokens: Vec<Token>,
    /// line -> (column, text including the `//`)
    pub comments: HashMap<u32, (u32, String)>,
    pub line_count: u32,
}

pub struct Lexer<'a> {
    src: Vec<char>,
    file: &'a str,
    i: usize,
    line: u32,
    col: u32,
    comments: HashMap<u32, (u32, String)>,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &str, file: &'a str) -> Lexer<'a> {
        Lexer { src: src.chars().collect(), file, i: 0, line: 1, col: 1, comments: HashMap::new() }
    }

    fn pos(&self) -> Pos {
        Pos::new(self.line, self.col)
    }

    fn err(&self, message: impl Into<String>, pos: Pos) -> HydraError {
        HydraError::new(message, self.file, pos)
    }

    fn at_end(&self) -> bool {
        self.i >= self.src.len()
    }

    fn peek(&self) -> Option<char> {
        self.src.get(self.i).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.src.get(self.i + ahead).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn bump_n(&mut self, n: usize) -> String {
        let mut out = String::new();
        for _ in 0..n {
            match self.bump() {
                Some(c) => out.push(c),
                None => break,
            }
        }
        out
    }

    fn starts_with(&self, text: &str) -> bool {
        text.chars().enumerate().all(|(k, c)| self.peek_at(k) == Some(c))
    }

    pub fn run(mut self) -> Result<Lexed> {
        let mut out: Vec<Token> = Vec::new();
        self.scan(&mut out, false)?;

        // A file that does not end in a newline still ends its last statement.
        if matches!(out.last(), Some(t) if !t.is_newline()) {
            out.push(Token::new(Tok::Newline, self.pos()));
        }
        let line_count = self.line.saturating_sub(if self.col == 1 { 1 } else { 0 }).max(1);
        out.push(Token::new(Tok::Eof, self.pos()));

        Ok(Lexed { tokens: out, comments: self.comments, line_count })
    }

    /// Lex into `out`.
    ///
    /// With `interpolation` set, the scan stops at the `)` that closes a
    /// `\(…)` — leaving the cursor on it — and a newline before that point is
    /// an error: string literals and interpolations stay on one physical line (§1).
    fn scan(&mut self, out: &mut Vec<Token>, interpolation: bool) -> Result<()> {
        let mut depth = 0i32;
        while !self.at_end() {
            let before = self.i;
            let produced = out.len();
            let c = self.peek().unwrap();

            if c.is_whitespace() && c != '\n' {
                self.bump();
                continue;
            }

            if c == '\n' {
                if interpolation {
                    return Err(
                        self.err("unterminated interpolation: newline inside \\(…)", self.pos())
                    );
                }
                let pos = self.pos();
                self.bump();
                out.push(Token::new(Tok::Newline, pos));
                set_len(out, produced, 1);
                continue;
            }

            if c == '/' && self.peek_at(1) == Some('/') {
                let pos = self.pos();
                let mut text = String::new();
                while let Some(ch) = self.peek() {
                    if ch == '\n' {
                        break;
                    }
                    text.push(ch);
                    self.bump();
                }
                self.comments.insert(pos.line, (pos.col, text.trim_end().to_string()));
                continue;
            }

            if interpolation && c == ')' && depth == 0 {
                return Ok(());
            }

            let pos = self.pos();

            if c == '"' {
                let parts = self.string()?;
                out.push(Token::new(Tok::Str { parts }, pos));
                set_len(out, produced, self.i - before);
                continue;
            }

            if c.is_ascii_digit() {
                out.push(self.number(pos));
                set_len(out, produced, self.i - before);
                continue;
            }

            if is_ident_start(c) {
                out.push(self.word(pos));
                set_len(out, produced, self.i - before);
                continue;
            }

            if c == ':' && !self.starts_with(":=")
                && (!self.starts_with("::") || matches!(out.last().map(|t| &t.kind), Some(Tok::Sym { .. })))
            {
                let tok = self.colon(pos, out.last())?;
                out.push(tok);
                set_len(out, produced, self.i - before);
                continue;
            }

            match self.operator() {
                Some(op) => {
                    match op {
                        "(" => depth += 1,
                        ")" => depth -= 1,
                        _ => {}
                    }
                    out.push(Token::new(Tok::Op(op), pos))
                }
                None => return Err(self.err(format!("unexpected character `{c}`"), pos)),
            }
            set_len(out, produced, self.i - before);
        }

        if interpolation {
            return Err(self.err("unterminated interpolation: missing `)`", self.pos()));
        }
        Ok(())
    }

    fn operator(&mut self) -> Option<&'static str> {
        for op in OPERATORS {
            if self.starts_with(op) {
                self.bump_n(op.chars().count());
                return Some(op);
            }
        }
        None
    }

    /// `digits [ "." digits ] [ ("e"|"E") ["+"|"-"] digits ]`.
    ///
    /// The fraction needs a digit after the dot so that `3.foo` stays a key
    /// lookup on the number `3`.
    fn number(&mut self, pos: Pos) -> Token {
        let mut raw = String::new();
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            raw.push(self.bump().unwrap());
        }
        if self.peek() == Some('.') && matches!(self.peek_at(1), Some(c) if c.is_ascii_digit()) {
            raw.push(self.bump().unwrap());
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                raw.push(self.bump().unwrap());
            }
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            let mut ahead = 1;
            if matches!(self.peek_at(ahead), Some('+') | Some('-')) {
                ahead += 1;
            }
            if matches!(self.peek_at(ahead), Some(c) if c.is_ascii_digit()) {
                raw.push_str(&self.bump_n(ahead));
                while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                    raw.push(self.bump().unwrap());
                }
            }
        }
        let value = raw.parse::<f64>().unwrap_or(f64::NAN);
        Token::new(Tok::Num { value, raw }, pos)
    }

    fn word(&mut self, pos: Pos) -> Token {
        let word = self.take_ident();

        // A compound keyword may be separated by spaces or tabs, never a
        // newline (§2).
        if COMPOUND.iter().any(|(head, _, _)| *head == word) {
            let save = (self.i, self.line, self.col);
            let mut gap = 0;
            while matches!(self.peek(), Some(' ') | Some('\t')) {
                self.bump();
                gap += 1;
            }
            if gap > 0 && matches!(self.peek(), Some(c) if is_ident_start(c)) {
                let next = self.take_ident();
                if let Some(&(_, _, spelling)) =
                    COMPOUND.iter().find(|(head, tail, _)| *head == word && *tail == next)
                {
                    return Token::new(Tok::Kw(spelling), pos);
                }
            }
            self.i = save.0;
            self.line = save.1;
            self.col = save.2;
        }

        match KEYWORDS.iter().find(|k| **k == word) {
            Some(k) => Token::new(Tok::Kw(k), pos),
            None => Token::new(Tok::Ident(word), pos),
        }
    }

    fn take_symbol_name(&mut self) -> String {
        let mut name = String::new();
        while self.peek().is_some_and(|c| !is_symbol_delimiter(c))
            && !self.starts_with("//")
            && !self.starts_with("||")
        {
            name.push(self.bump().unwrap());
        }
        name
    }

    fn take_ident(&mut self) -> String {
        let mut word = String::new();
        while matches!(self.peek(), Some(c) if is_ident_char(c)) {
            word.push(self.bump().unwrap());
        }
        word
    }

    /// A colon followed immediately by a name or quote opens an atom in
    /// expression-leading position. After an atom it is the dict separator,
    /// so even `{:key::value}` remains an unambiguous compact dict.
    fn colon(&mut self, pos: Pos, prev: Option<&Token>) -> Result<Token> {
        let after_value = match prev.map(|t| &t.kind) {
            Some(Tok::Ident(_)) | Some(Tok::Num { .. }) | Some(Tok::Str { .. })
            | Some(Tok::Sym { .. }) => true,
            Some(Tok::Op(o)) => matches!(*o, ")" | "]" | "}"),
            _ => false,
        };
        self.bump();
        if after_value {
            return Ok(Token::new(Tok::Op(":"), pos));
        }
        if self.peek() == Some('"') {
            let parts = self.string()?;
            return Ok(Token::new(Tok::Sym { parts, quoted: true }, pos));
        }
        if !matches!(self.peek(), Some(c) if is_ident_start(c)) {
            return Ok(Token::new(Tok::Op(":"), pos));
        }
        let name = self.take_symbol_name();
        Ok(Token::new(Tok::Sym { parts: vec![text_piece(&name)], quoted: false }, pos))
    }

    /// Escapes are `\" \\ \n \t \r \0 \(` (§1); `\(` opens an interpolation,
    /// which is lexed recursively and may contain strings of its own.
    fn string(&mut self) -> Result<Vec<StrPiece>> {
        let start = self.pos();
        self.bump(); // opening quote
        let mut parts: Vec<StrPiece> = Vec::new();
        let mut value = String::new();
        let mut raw = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.err("unterminated string literal", start));
            };
            if c == '\n' {
                return Err(self.err("unterminated string literal", start));
            }
            if c == '"' {
                self.bump();
                break;
            }
            if c == '\\' {
                let esc_pos = self.pos();
                if self.peek_at(1) == Some('(') {
                    if !value.is_empty() || !raw.is_empty() {
                        parts.push(StrPiece::Text {
                            value: std::mem::take(&mut value),
                            raw: std::mem::take(&mut raw),
                        });
                    }
                    self.bump_n(2); // the `\(`
                    let mut tokens: Vec<Token> = Vec::new();
                    self.scan(&mut tokens, true)?;
                    if self.peek() != Some(')') {
                        return Err(self.err("unterminated interpolation: missing `)`", esc_pos));
                    }
                    self.bump(); // the `)`
                    tokens.push(Token::new(Tok::Eof, self.pos()));
                    parts.push(StrPiece::Expr { tokens, pos: esc_pos });
                    continue;
                }
                let decoded = match self.peek_at(1) {
                    Some('"') => '"',
                    Some('\\') => '\\',
                    Some('n') => '\n',
                    Some('t') => '\t',
                    Some('r') => '\r',
                    Some('0') => '\0',
                    Some(other) => {
                        return Err(self.err(format!("unknown escape `\\{other}`"), esc_pos))
                    }
                    None => return Err(self.err("unterminated string literal", start)),
                };
                value.push(decoded);
                raw.push_str(&self.bump_n(2));
                continue;
            }
            value.push(c);
            raw.push(self.bump().unwrap());
        }
        if !value.is_empty() || !raw.is_empty() || parts.is_empty() {
            parts.push(StrPiece::Text { value, raw });
        }
        Ok(parts)
    }
}

/// Record how many source characters the token just pushed spans.
fn set_len(out: &mut [Token], produced: usize, len: usize) {
    if out.len() > produced {
        out[produced].len = len as u32;
    }
}

pub fn tokenize(src: &str, file: &str) -> Result<Lexed> {
    Lexer::new(src, file).run()
}
