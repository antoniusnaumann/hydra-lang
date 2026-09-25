//! Editor support (spec §13).
//!
//! The token classes for semantic highlighting, and a TextMate grammar
//! generated from the same table so the two cannot drift apart.
//!
//! The classification runs on the real token stream, which is what makes the
//! one interesting rule in §13 free: `parallel for` is **one** token, so it is
//! coloured as one unit rather than as a concurrency keyword next to a control
//! keyword. A regex grammar has to work to get that right; here it is what the
//! lexer already decided.

use crate::errors::Result;
use crate::lexer::{tokenize, Tok, Token};

/// The classes of §13, with the reference Dark+ colours from the mock-ups.
pub const CLASSES: &[(&str, &str, &str)] = &[
    ("keyword.concurrency", "#ff8a65", "bold"),
    ("punctuation.trail", "#ff8a6580", ""),
    ("keyword.control", "#c586c0", ""),
    ("keyword.other", "#569cd6", ""),
    ("punctuation.delimiter", "#d4d4d4", ""),
    ("entity.namespace", "#4ec9b0", ""),
    ("variable", "#9cdcfe", ""),
    ("variable.property", "#9cdcfe", ""),
    ("entity.function", "#dcdcaa", ""),
    ("constant", "#4fc1ff", ""),
    ("string", "#ce9178", ""),
    ("constant.numeric", "#b5cea8", ""),
    ("comment", "#6a9955", "italic"),
];

const CONTROL: &[&str] =
    &["if", "else", "else if", "for", "in", "while", "return", "end"];
const CONTROL_HANDLERS: &[&str] = &["exit", "panic", "reject"];
const OTHER: &[&str] = &["fn", "use", "and", "or", "not", "as"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticToken {
    pub line: u32,
    pub col: u32,
    pub len: u32,
    pub class: &'static str,
}

/// `ALL_CAPS` is convention only and has no semantics (§2) — but §13 does give
/// it a colour.
fn is_all_caps(name: &str) -> bool {
    name.chars().any(|c| c.is_ascii_uppercase())
        && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Classify one token. `prev` and `next` decide the three context-sensitive
/// cases: a property after `.`, a module before `::`, and a call.
pub fn class_of(tok: &Token, prev: Option<&Token>, next: Option<&Token>) -> Option<&'static str> {
    match &tok.kind {
        Tok::Newline | Tok::Eof => None,
        Tok::Str { .. } => Some("string"),
        Tok::Num { .. } => Some("constant.numeric"),
        Tok::Sym { .. } => Some(if tok.static_str().is_some_and(|name| CONTROL_HANDLERS.contains(&name.as_str())) {
            "keyword.control"
        } else {
            "constant"
        }),
        Tok::Kw(kw) => Some(if kw.starts_with("parallel") || kw.starts_with("race") {
            // `parallel`, `race`, and every compound built on them, coloured as
            // one unit (§13).
            "keyword.concurrency"
        } else if CONTROL.contains(kw) {
            "keyword.control"
        } else {
            // §13 lists `fn use and or not as` here, and there is no keyword
            // outside the three groups.
            "keyword.other"
        }),
        Tok::Op(op) => {
            if *op == "||" {
                Some("punctuation.trail")
            } else {
                None
            }
        }
        Tok::Ident(name) => {
            if next.map(|t| t.is_op("::")).unwrap_or(false) {
                return Some("entity.namespace");
            }
            if next.map(|t| t.is_op("(")).unwrap_or(false) {
                return Some(if CONTROL_HANDLERS.contains(&name.as_str()) && !prev.is_some_and(|t| t.is_op(".")) { "keyword.control" } else { "entity.function" });
            }
            if prev.map(|t| t.is_op(".")).unwrap_or(false) {
                return Some("variable.property");
            }
            if is_all_caps(name) {
                return Some("constant");
            }
            Some("variable")
        }
    }
}

/// Every classified token in a file, in source order, ready to be turned into
/// LSP semantic tokens.
pub fn semantic_tokens(src: &str, file: &str) -> Result<Vec<SemanticToken>> {
    let lexed = tokenize(src, file)?;
    let mut out: Vec<SemanticToken> = Vec::new();
    for (i, tok) in lexed.tokens.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| &lexed.tokens[j]);
        let next = lexed.tokens.get(i + 1);
        if let Tok::Sym { .. } = &tok.kind {
            out.push(SemanticToken { line: tok.pos.line, col: tok.pos.col, len: 1, class: "punctuation.delimiter" });
            out.push(SemanticToken { line: tok.pos.line, col: tok.pos.col + 1, len: tok.len - 1, class: class_of(tok, prev, next).expect("atom class") });
        } else if let Some(mut class) = class_of(tok, prev, next) {
            if class == "keyword.control" && prev.is_some_and(|t| t.is_op("::"))
                && i.checked_sub(2).is_some_and(|j| matches!(lexed.tokens[j].kind, Tok::Ident(_))) {
                class = "entity.function";
            }
            out.push(SemanticToken { line: tok.pos.line, col: tok.pos.col, len: tok.len, class });
        }
    }
    for (line, (col, text)) in &lexed.comments {
        out.push(SemanticToken {
            line: *line,
            col: *col,
            len: text.chars().count() as u32,
            class: "comment",
        });
    }
    out.sort_by_key(|t| (t.line, t.col));
    Ok(out)
}

/// A TextMate grammar, generated from the same tables the classifier uses.
pub fn tmlanguage_json() -> String {
    let mut patterns: Vec<String> = Vec::new();

    patterns.push(rule("comment", "//.*$"));
    patterns.push(rule("string", r#"\"(\\\\.|[^\"\\\\])*\""#));

    // The compound keywords must come first, so that `parallel for` matches as
    // one unit rather than `parallel` followed by `for` (§13).
    let compound: Vec<String> = crate::lexer::COMPOUND
        .iter()
        .filter(|(head, _, _)| *head == "parallel" || *head == "race")
        .map(|(head, tail, _)| format!("{head}[ \\t]+{tail}"))
        .collect();
    patterns.push(rule("keyword.concurrency", &format!("\\b({})\\b", compound.join("|"))));
    patterns.push(rule("keyword.concurrency", "\\b(parallel|race)\\b"));
    patterns.push(rule("keyword.control", "\\belse[ \\t]+if\\b"));
    patterns.push(rule("keyword.control", &format!("\\b({})\\b", CONTROL.join("|"))));
    patterns.push(rule("keyword.other", &format!("\\b({})\\b", OTHER.join("|"))));

    patterns.push(rule("punctuation.trail", r"\|\|"));
    let qualified_control = r"(?<![\w.:])(::)(exit|panic|reject)(?=[ \t]*\()";
    patterns.push(format!(
        "    {{ \"match\": \"{}\", \"captures\": {{ \"1\": {{ \"name\": \"punctuation.delimiter.hydra\" }}, \"2\": {{ \"name\": \"keyword.control.hydra\" }} }} }}",
        escape_json(qualified_control)
    ));
    patterns.push(rule("punctuation.delimiter", "::"));
    patterns.push(rule("keyword.control", r"(?<![\w.:])(?:exit|panic|reject)(?=[ \t]*\()"));
    let control_atom = r#"(:)(\"exit\"|\"panic\"|\"reject\"|(?:exit|panic|reject)(?=$|[\s()\[\]{},:\"]|//|\|\|))(:)?"#;
    patterns.push(format!(
        "    {{ \"match\": \"{}\", \"captures\": {{ \"1\": {{ \"name\": \"punctuation.delimiter.hydra\" }}, \"2\": {{ \"name\": \"keyword.control.hydra\" }}, \"3\": {{ \"name\": \"punctuation.delimiter.hydra\" }} }} }}",
        escape_json(control_atom)
    ));
    // Match the prefix and payload separately so only the atom text is constant.
    let atom = r#"(:)(\"[^\"]*\"|[A-Za-z_](?:(?!//|\|\|)[^\s()\[\]{},:\"])*)(:)?"#;
    patterns.push(format!(
        "    {{ \"match\": \"{}\", \"captures\": {{ \"1\": {{ \"name\": \"punctuation.delimiter.hydra\" }}, \"2\": {{ \"name\": \"constant.hydra\" }}, \"3\": {{ \"name\": \"punctuation.delimiter.hydra\" }} }} }}",
        escape_json(atom)
    ));
    patterns.push(rule("punctuation.delimiter", ":"));
    patterns.push(rule("entity.namespace", "[A-Za-z_][A-Za-z0-9_]*(?=::)"));
    patterns.push(rule("entity.function", r"[A-Za-z_][A-Za-z0-9_]*(?=\()"));
    patterns.push(rule("constant.numeric", r"\b[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?\b"));
    patterns.push(rule("constant", "\\b[A-Z][A-Z0-9_]*\\b"));
    patterns.push(rule("variable", "[A-Za-z_][A-Za-z0-9_]*"));

    format!(
        "{{\n  \"name\": \"Hydra\",\n  \"scopeName\": \"source.hydra\",\n  \
         \"fileTypes\": [\"hy\"],\n  \"patterns\": [\n{}\n  ]\n}}\n",
        patterns.join(",\n")
    )
}

fn rule(class: &str, pattern: &str) -> String {
    format!("    {{ \"name\": \"{class}.hydra\", \"match\": \"{}\" }}", escape_json(pattern))
}

fn escape_json(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            other => out.push(other),
        }
    }
    out
}

/// The colours of §13, as a theme fragment.
pub fn theme_json() -> String {
    let entries: Vec<String> = CLASSES
        .iter()
        .map(|(class, colour, style)| {
            let font = if style.is_empty() {
                String::new()
            } else {
                format!(", \"fontStyle\": \"{style}\"")
            };
            format!(
                "    {{ \"scope\": \"{class}.hydra\", \"settings\": {{ \"foreground\": \"{colour}\"{font} }} }}"
            )
        })
        .collect();
    format!("{{\n  \"textMateRules\": [\n{}\n  ]\n}}\n", entries.join(",\n"))
}
