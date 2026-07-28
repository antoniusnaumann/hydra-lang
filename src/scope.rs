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

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::value::{Cell, Value};

pub type ScopeRef = Rc<Scope>;

impl std::fmt::Debug for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Scope{:?}", self.names())
    }
}

pub struct Scope {
    vars: RefCell<HashMap<Rc<str>, Cell>>,
    parent: Option<ScopeRef>,
}

impl Scope {
    pub fn root() -> ScopeRef {
        Rc::new(Scope { vars: RefCell::new(HashMap::new()), parent: None })
    }

    pub fn child(parent: &ScopeRef) -> ScopeRef {
        Rc::new(Scope { vars: RefCell::new(HashMap::new()), parent: Some(parent.clone()) })
    }

    pub fn parent(&self) -> Option<&ScopeRef> {
        self.parent.as_ref()
    }

    /// `x := expr` — a fresh binding in this scope, shadowing any outer one.
    pub fn declare(&self, name: &str, value: Value) -> Cell {
        let cell = Rc::new(RefCell::new(value));
        self.vars.borrow_mut().insert(Rc::from(name), cell.clone());
        cell
    }

    /// The cell for `name` in this scope only.
    pub fn get_local(&self, name: &str) -> Option<Cell> {
        self.vars.borrow().get(name).cloned()
    }

    /// The cell for `name`, searching outward through the chain (§6).
    pub fn lookup(&self, name: &str) -> Option<Cell> {
        if let Some(cell) = self.get_local(name) {
            return Some(cell);
        }
        let mut scope = self.parent.clone();
        while let Some(s) = scope {
            if let Some(cell) = s.get_local(name) {
                return Some(cell);
            }
            scope = s.parent.clone();
        }
        None
    }

    /// Bind an existing cell under a name — how `use` shares a module's own
    /// storage rather than a copy of it (§7).
    pub fn bind_cell(&self, name: &str, cell: Cell) {
        self.vars.borrow_mut().insert(Rc::from(name), cell);
    }

    /// Every name bound directly in this scope, for `use` and for tests.
    pub fn names(&self) -> Vec<Rc<str>> {
        let mut names: Vec<Rc<str>> = self.vars.borrow().keys().cloned().collect();
        names.sort();
        names
    }
}
