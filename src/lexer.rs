//! The lexer (spec §1 and §2).
//!
//! Two things here are not the usual thing:
//!
//! * **Compound keywords.** `else if`, `parallel for`, `parallel while`,
//!   `race for` and `race while` lex as *one* token. The internal whitespace is
//!   spaces or tabs, never a newline, so `else` at end of line followed by `if`
//!   on the next line is two tokens and means something different (§2).
//!
//! * **Symbol versus key lookup.** A dot in leading position opens a symbol; a
//!   dot directly after an expression is a lookup. The lexer decides from the
//!   preceding token, which is why that decision lives here and not in the
//!   parser.
//!
//! Strings are plain: there is no interpolation, `+` concatenates. They can
//! still contain `||`, `end` or `//`, so everything downstream works on tokens
//! rather than on text.

use std::collections::HashMap;
use std::fmt;

use crate::errors::{HydraError, Pos, Result};

/// Keywords (§2). `trail` is not here: it is a reserved *label*, meaningful
/// only after `break` (§9.6), so the parser gives it meaning.
pub const KEYWORDS: &[&str] = &[
    "fn", "use", "if", "else", "for", "in", "while", "return", "break", "continue", "end", "and",
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
/// before `>>` before `>`. The table is written longest-first so that the rule
/// is visible rather than implied.
pub const OPERATORS: &[&str] = &[
    ">>>", "===", "!==", //
    "==", "!=", "<=", ">=", "<<", ">>", ":=", "::", "||", //
    "=", "<", ">", "+", "-", "*", "/", "%", //
    "|", "&", "^", "~", //
    "(", ")", "[", "]", "{", "}", ",", ".", ":",
];

/// The trail separator (§2). Not logical or — Hydra has no `||` operator.
pub const TRAIL_SEP: &str = "||";

/// `break trail` targets the innermost trail; `trail` is a reserved label.
pub const TRAIL_LABEL: &str = "trail";

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Newline,
    Eof,
    Ident(String),
    Num { value: f64, raw: String },
    /// `raw` is the spelling *between* the quotes, so a format pass can put the
    /// escapes back exactly as they were written.
    Str { value: String, raw: String },
    Sym { name: String, quoted: bool },
    Kw(&'static str),
    Op(&'static str),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: Tok,
    pub pos: Pos,
}

impl Token {
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

    /// The canonical spelling of this token, which is what the formatter emits.
    pub fn text(&self) -> String {
        match &self.kind {
            Tok::Newline => "\n".into(),
            Tok::Eof => String::new(),
            Tok::Ident(name) => name.clone(),
            Tok::Num { raw, .. } => raw.clone(),
            Tok::Str { raw, .. } => format!("\"{raw}\""),
            Tok::Sym { name, quoted } => {
                // §12 rule 3a: a quoted symbol whose content is a valid
                // identifier is rewritten bare.
                if *quoted && !is_identifier(name) {
                    format!(".\"{}\"", escape_string(name))
                } else {
                    format!(".{name}")
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
            other => write!(f, "`{}`", Token { kind: other.clone(), pos: self.pos }.text()),
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

        while !self.at_end() {
            let c = self.peek().unwrap();

            if c == ' ' || c == '\t' || c == '\r' {
                self.bump();
                continue;
            }

            if c == '\n' {
                let pos = self.pos();
                self.bump();
                out.push(Token { kind: Tok::Newline, pos });
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

            let pos = self.pos();

            if c == '"' {
                let (value, raw) = self.string()?;
                out.push(Token { kind: Tok::Str { value, raw }, pos });
                continue;
            }

            if c.is_ascii_digit() {
                out.push(self.number(pos));
                continue;
            }

            if is_ident_start(c) {
                out.push(self.word(pos));
                continue;
            }

            if c == '.' {
                let tok = self.dot(pos, out.last())?;
                out.push(tok);
                continue;
            }

            match self.operator() {
                Some(op) => out.push(Token { kind: Tok::Op(op), pos }),
                None => return Err(self.err(format!("unexpected character `{c}`"), pos)),
            }
        }

        // A file that does not end in a newline still ends its last statement.
        if matches!(out.last(), Some(t) if !t.is_newline()) {
            out.push(Token { kind: Tok::Newline, pos: self.pos() });
        }
        let line_count = self.line.saturating_sub(if self.col == 1 { 1 } else { 0 }).max(1);
        out.push(Token { kind: Tok::Eof, pos: self.pos() });

        Ok(Lexed { tokens: out, comments: self.comments, line_count })
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
        Token { kind: Tok::Num { value, raw }, pos }
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
                    return Token { kind: Tok::Kw(spelling), pos };
                }
            }
            self.i = save.0;
            self.line = save.1;
            self.col = save.2;
        }

        match KEYWORDS.iter().find(|k| **k == word) {
            Some(k) => Token { kind: Tok::Kw(k), pos },
            None => Token { kind: Tok::Ident(word), pos },
        }
    }

    fn take_ident(&mut self) -> String {
        let mut word = String::new();
        while matches!(self.peek(), Some(c) if is_ident_char(c)) {
            word.push(self.bump().unwrap());
        }
        word
    }

    /// A dot in leading position starts a symbol; a dot directly after an
    /// expression is a key lookup (§2).
    fn dot(&mut self, pos: Pos, prev: Option<&Token>) -> Result<Token> {
        let lookup = match prev.map(|t| &t.kind) {
            Some(Tok::Ident(_)) | Some(Tok::Num { .. }) | Some(Tok::Str { .. })
            | Some(Tok::Sym { .. }) => true,
            Some(Tok::Op(o)) => matches!(*o, ")" | "]" | "}"),
            _ => false,
        };
        if lookup {
            self.bump();
            return Ok(Token { kind: Tok::Op("."), pos });
        }

        self.bump(); // the dot
        if self.peek() == Some('"') {
            let (value, _) = self.string()?;
            return Ok(Token { kind: Tok::Sym { name: value, quoted: true }, pos });
        }
        if !matches!(self.peek(), Some(c) if is_ident_start(c)) {
            return Err(self.err("expected a name or a quoted string after `.`", pos));
        }
        let name = self.take_ident();
        Ok(Token { kind: Tok::Sym { name, quoted: false }, pos })
    }

    /// Escapes are `\" \\ \n \t \r \0` (§1).
    fn string(&mut self) -> Result<(String, String)> {
        let start = self.pos();
        self.bump(); // opening quote
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
        Ok((value, raw))
    }
}

pub fn tokenize(src: &str, file: &str) -> Result<Lexed> {
    Lexer::new(src, file).run()
}
