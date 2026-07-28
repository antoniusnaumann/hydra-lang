//! Scopes and bindings (spec §6).
//!
//! A scope is a hash map plus a parent pointer. Scopes themselves are never
//! copied — it is the *values* flowing between bindings that copy (§5.1), so a
//! closure that captures a scope chain sees later writes through it, and a
//! trail writes its parent's bindings directly.
//!
//! A binding is a [`Cell`]: the storage a name refers to. Cells are what `&`
//! points at, which is why `x := …` installs a *fresh* cell (shadowing, so
//! closures holding the old one keep the old one) while `x = …` writes into the
//! cell an outward search finds.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::value::{Cell, Value};

pub type ScopeRef = Arc<Scope>;

impl std::fmt::Debug for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Scope{:?}", self.names())
    }
}

pub struct Scope {
    vars: RwLock<HashMap<Arc<str>, Cell>>,
    parent: Option<ScopeRef>,
}

impl Scope {
    pub fn root() -> ScopeRef {
        Arc::new(Scope { vars: RwLock::new(HashMap::new()), parent: None })
    }

    pub fn child(parent: &ScopeRef) -> ScopeRef {
        Arc::new(Scope { vars: RwLock::new(HashMap::new()), parent: Some(parent.clone()) })
    }

    pub fn parent(&self) -> Option<&ScopeRef> {
        self.parent.as_ref()
    }

    /// `x := expr` — a fresh binding in this scope, shadowing any outer one.
    pub fn declare(&self, name: &str, value: Value) -> Cell {
        let cell = Arc::new(RwLock::new(value));
        self.vars.write().unwrap_or_else(|e| e.into_inner()).insert(Arc::from(name), cell.clone());
        cell
    }

    /// The cell for `name` in this scope only.
    pub fn get_local(&self, name: &str) -> Option<Cell> {
        self.vars.read().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
    }

    /// The cell for `name`, searching outward through the chain (§6).
    ///
    /// The walk borrows rather than cloning each `Arc`: an outer scope is
    /// shared by every trail under it, and bumping its refcount on every
    /// variable read would put one cache line in the path of all of them.
    pub fn lookup(&self, name: &str) -> Option<Cell> {
        let mut scope = Some(self);
        while let Some(s) = scope {
            if let Some(cell) = s.get_local(name) {
                return Some(cell);
            }
            scope = s.parent.as_deref();
        }
        None
    }

    /// Every binding of `name` in the chain, innermost first.
    ///
    /// There can be more than one: `x := …` on a name that already exists is a
    /// fresh binding that shadows (§6). A *call* tries them in this order and
    /// takes the first that accepts it, so shadowing a function with one of a
    /// different shape does not hide the original (§3).
    pub fn all_bindings(&self, name: &str) -> Vec<Cell> {
        let mut out = Vec::new();
        let mut scope = Some(self);
        while let Some(s) = scope {
            if let Some(cell) = s.get_local(name) {
                out.push(cell);
            }
            scope = s.parent.as_deref();
        }
        out
    }

    /// Bind an existing cell under a name — how `use` shares a module's own
    /// storage rather than a copy of it (§7).
    pub fn bind_cell(&self, name: &str, cell: Cell) {
        self.vars.write().unwrap_or_else(|e| e.into_inner()).insert(Arc::from(name), cell);
    }

    /// Every name bound directly in this scope, for `use` and for tests.
    pub fn names(&self) -> Vec<Arc<str>> {
        let mut names: Vec<Arc<str>> =
            self.vars.read().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
        names.sort();
        names
    }
}
