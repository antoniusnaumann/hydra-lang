//! Trails, cancellation flags and the scheduler's bookkeeping (spec §9).
//!
//! Trails are green threads: a trail is a [`Task`], which is a stack of frames
//! and an operand stack, so suspending one costs nothing and there is no OS
//! thread and no memory model anywhere (§9.1's recommendation).
//!
//! Cancellation flags form a tree: a child holds a pointer to its parent's flag
//! and asks by walking up (§10 item 4), so cancelling a trail cancels
//! everything inside it — including a `parallel` block a called function opened
//! — without broadcasting to anyone.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::ast::BlockKind;
use crate::compile::Chunk;
use crate::errors::Pos;
use crate::scope::ScopeRef;
use crate::value::{ListRef, Value};

pub type TaskId = u64;
pub type BlockId = u64;

/// A trail's liveness. `alive()` reads it; cancellation sets it (§9.5).
pub struct CancelFlag {
    cancelled: AtomicBool,
    parent: Option<Arc<CancelFlag>>,
}

impl CancelFlag {
    pub fn root() -> Arc<CancelFlag> {
        Arc::new(CancelFlag { cancelled: AtomicBool::new(false), parent: None })
    }

    pub fn child(parent: &Arc<CancelFlag>) -> Arc<CancelFlag> {
        Arc::new(CancelFlag { cancelled: AtomicBool::new(false), parent: Some(parent.clone()) })
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Cancellation propagates: everything inside a dead trail is dead (§9.5).
    ///
    /// A trail asks by walking up its own chain, so cancelling one is a single
    /// store and never a broadcast — which matters more with real threads, not
    /// less.
    pub fn is_cancelled(&self) -> bool {
        if self.cancelled.load(Ordering::Relaxed) {
            return true;
        }
        let mut parent = self.parent.as_ref();
        while let Some(flag) = parent {
            if flag.cancelled.load(Ordering::Relaxed) {
                return true;
            }
            parent = flag.parent.as_ref();
        }
        false
    }
}

/// Iterating a list iterates the value it was, not a live view of it: the
/// handle is shared, so a write elsewhere path-copies away from it (§5.1).
pub struct IterState {
    pub list: ListRef,
    pub index: usize,
}

/// What to do with a frame's result when it returns.
pub enum OnReturn {
    /// Ordinary call: the value lands on the caller's operand stack.
    PushValue,
    /// A module body ran; bind its public names into the importer (§7).
    BindModule { alias: Arc<str>, module: usize },
}

/// One level of a frame's scope stack.
///
/// `dynamic` marks a level that `:=` opened because the name it declares was
/// already bound in this scope. §6 says such a redeclaration is a *fresh*
/// binding and that closures which captured the old one keep the old one, so
/// the new binding has to live in a level of its own rather than replacing the
/// entry a captured scope can still see.
pub struct ScopeSlot {
    pub scope: ScopeRef,
    pub dynamic: bool,
}

pub struct Frame {
    pub chunk: Arc<Chunk>,
    pub ip: usize,
    /// Innermost last. Each `PushScope` opens a child of the current one (§6).
    pub scopes: Vec<ScopeSlot>,
    pub iters: Vec<IterState>,
    pub stack_base: usize,
    pub module: usize,
    pub on_return: OnReturn,
    pub call_site: Pos,
    /// A module's toplevel (including the main program's): its final scope is
    /// the namespace `use` exports from (§7).
    pub is_module_body: bool,
    /// Which parameters the call supplied, so the prologue knows which
    /// defaults to evaluate. Named arguments can leave holes, so this is a
    /// mask and not a count.
    pub provided: Vec<bool>,
}

impl Frame {
    pub fn scope(&self) -> &ScopeRef {
        &self.scopes.last().expect("a frame always has a scope").scope
    }

    pub fn push_scope(&mut self, scope: ScopeRef, dynamic: bool) {
        self.scopes.push(ScopeSlot { scope, dynamic });
    }

    /// Leave `n` static scopes, dropping any shadowing levels above them.
    pub fn pop_scopes(&mut self, n: usize) {
        let mut left = n;
        while left > 0 && self.scopes.len() > 1 {
            let slot = self.scopes.pop().expect("scope");
            if !slot.dynamic {
                left -= 1;
            }
        }
    }
}

/// A `parallel` / `race` block opened by a task (§9.3, §9.4).
///
/// It lives in the scheduler rather than in the opening task, because a trail
/// can finish while its parent is being stepped by another worker — and a
/// parent that is running has been taken out of the task table, so there would
/// be nowhere to record the completion.
pub struct BlockCtx {
    pub kind: BlockKind,
    pub owner: TaskId,
    pub children: Vec<TaskId>,
    pub pending: usize,
    /// Set when a `race` has been decided by its first completion.
    pub decided: bool,
}

impl BlockCtx {
    /// Control passes `end` when every trail has finished (§9.3), or at the
    /// first completion for a `race` (§9.4).
    pub fn satisfied(&self) -> bool {
        match self.kind {
            BlockKind::Parallel => self.pending == 0,
            BlockKind::Race => self.decided || self.pending == 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    /// Waiting at a `JoinBlock`.
    Blocked,
}

pub struct Task {
    pub id: TaskId,
    pub frames: Vec<Frame>,
    pub stack: Vec<Value>,
    pub cancel: Arc<CancelFlag>,
    /// The block this trail belongs to, if it is one.
    pub block: Option<BlockId>,
    /// Blocks this task has opened and not yet joined, innermost last.
    pub blocks: Vec<BlockId>,
    pub state: TaskState,
    /// A trail stops at a statement boundary of *its own* body, never inside a
    /// call it made: an in-flight call runs to the end (§9.5).
    pub base_depth: usize,
    pub is_trail: bool,
    pub module: usize,
}

impl Task {
    pub fn push(&mut self, value: Value) {
        self.stack.push(value);
    }

    pub fn pop(&mut self) -> Value {
        self.stack.pop().expect("operand stack underflow")
    }

    pub fn frame(&self) -> &Frame {
        self.frames.last().expect("a running task has a frame")
    }

    pub fn frame_mut(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("a running task has a frame")
    }

    pub fn scope(&self) -> &ScopeRef {
        self.frame().scope()
    }

    /// True where a cancelled trail must stop: at its own statement
    /// boundaries, not inside a call (§9.5).
    pub fn at_own_statement(&self) -> bool {
        self.frames.len() == self.base_depth
    }

    pub fn should_stop(&self) -> bool {
        self.at_own_statement() && self.cancel.is_cancelled()
    }
}

/// The run queue. Round-robin, so scheduling is deterministic and a test can
/// rely on it.
#[derive(Default)]
pub struct RunQueue {
    queue: VecDeque<TaskId>,
}

impl RunQueue {
    pub fn push(&mut self, id: TaskId) {
        self.queue.push_back(id);
    }

    pub fn pop(&mut self) -> Option<TaskId> {
        self.queue.pop_front()
    }
}
