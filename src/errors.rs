//! Positions, diagnostics, and the two kinds of failure Hydra has.
//!
//! A *compile-time* failure ([`HydraError`]) stops the tool before anything
//! runs. A *crash* ([`Crash`], spec §8) happens during evaluation: it kills the
//! program unless it happened inside a dead trail (§9.5), where it is isolated.

use std::fmt;

/// A source position. Line and column are 1-based; `0` means "unknown".
///
/// Positions survive the parallel transposition (§4 step 6): a token keeps the
/// position it had in the original file, never the one it would have in the
/// rearranged per-trail stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

impl Pos {
    pub const NONE: Pos = Pos { line: 0, col: 0 };

    pub fn new(line: u32, col: u32) -> Pos {
        Pos { line, col }
    }

    pub fn is_known(&self) -> bool {
        self.line != 0
    }
}

impl fmt::Display for Pos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

/// Where a diagnostic points: a file plus a position inside it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Site {
    pub file: String,
    pub pos: Pos,
}

impl Site {
    pub fn new(file: impl Into<String>, pos: Pos) -> Site {
        Site { file: file.into(), pos }
    }
}

impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.pos.is_known() {
            write!(f, "{}:{}", self.file, self.pos)
        } else {
            write!(f, "{}", self.file)
        }
    }
}

/// A compile-time error: lexing, parsing, or module resolution.
#[derive(Clone, Debug)]
pub struct HydraError {
    pub message: String,
    pub site: Site,
}

impl HydraError {
    pub fn new(message: impl Into<String>, file: &str, pos: Pos) -> HydraError {
        HydraError { message: message.into(), site: Site::new(file, pos) }
    }
}

impl fmt::Display for HydraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: error: {}", self.site, self.message)
    }
}

impl std::error::Error for HydraError {}

pub type Result<T> = std::result::Result<T, HydraError>;

/// A runtime crash (§8): missing field, bad operand, `=` to an undeclared name,
/// arity mismatch, explicit abort.
///
/// The trace is filled in as the crash unwinds through call frames so the
/// diagnostic can point at the statement that failed as well as at the frame
/// that raised it.
#[derive(Clone, Debug)]
pub struct Crash {
    pub message: String,
    pub site: Site,
    pub trace: Vec<Site>,
}

impl Crash {
    pub fn new(message: impl Into<String>) -> Crash {
        Crash { message: message.into(), site: Site::default(), trace: Vec::new() }
    }

    pub fn at(message: impl Into<String>, file: &str, pos: Pos) -> Crash {
        Crash { message: message.into(), site: Site::new(file, pos), trace: Vec::new() }
    }
}

impl fmt::Display for Crash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: crash: {}", self.site, self.message)?;
        for frame in &self.trace {
            write!(f, "\n    called from {frame}")?;
        }
        Ok(())
    }
}

/// One finding from `check` (§11).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Error => write!(f, "error"),
            Severity::Warning => write!(f, "warning"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub site: Site,
    /// A short stable name for the rule, so diagnostics can be grepped for.
    pub code: &'static str,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>, site: Site, code: &'static str) -> Diagnostic {
        Diagnostic { severity: Severity::Error, message: message.into(), site, code }
    }

    pub fn warning(message: impl Into<String>, site: Site, code: &'static str) -> Diagnostic {
        Diagnostic { severity: Severity::Warning, message: message.into(), site, code }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}: {} [{}]", self.site, self.severity, self.message, self.code)
    }
}

/// The collected output of a `check` run.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub diagnostics: Vec<Diagnostic>,
}

impl Report {
    pub fn error(&mut self, message: impl Into<String>, site: Site, code: &'static str) {
        self.diagnostics.push(Diagnostic::error(message, site, code));
    }

    pub fn warn(&mut self, message: impl Into<String>, site: Site, code: &'static str) {
        self.diagnostics.push(Diagnostic::warning(message, site, code));
    }

    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter().filter(|d| d.severity == Severity::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter().filter(|d| d.severity == Severity::Warning)
    }

    pub fn has_errors(&self) -> bool {
        self.errors().next().is_some()
    }

    /// Source order, so a run reads top to bottom.
    pub fn sorted(&self) -> Vec<Diagnostic> {
        let mut out = self.diagnostics.clone();
        out.sort_by(|a, b| {
            (&a.site.file, a.site.pos, a.severity).cmp(&(&b.site.file, b.site.pos, b.severity))
        });
        out
    }

    pub fn codes(&self) -> Vec<&'static str> {
        self.diagnostics.iter().map(|d| d.code).collect()
    }
}
