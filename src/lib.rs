//! Hydra — a reference implementation of the language in `spec/hydra_spec.md`.
//!
//! The crate is split the way the spec is: lexer and parser (§1–§4), values
//! (§5), the interpreter and its scheduler (§6–§10), `check` (§11) and the
//! formatter (§12).

pub mod ast;
pub mod check;
pub mod compile;
pub mod editor;
pub mod errors;
pub mod format;
pub mod fs;
pub mod lexer;
pub mod parser;
pub mod scope;
pub mod sched;
pub mod value;
pub mod vm;

mod stdlib;
mod http;
