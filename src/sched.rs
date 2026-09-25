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
    /// A module body ran; bind it into the importer under the rules of its
    /// `use` (§7).
    BindModule { name: Arc<str>, alias: Arc<str>, unqualified: bool, module: usize },
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
    /// Where a `:reject` this frame answers with goes (§3): the candidates the
    /// call has not tried yet, the arguments to try them with, and what the
    /// ones before it said on the way out. Absent for a module body or a
    /// trail, which no call opened.
    pub retry: Option<Box<Retry>>,
}

/// One candidate's refusal, kept for the crash that reports them all (§3).
pub struct RejectedBy {
    pub signature: String,
    pub message: Option<String>,
    /// Refusals from a helper whose unconsumed result rejected this candidate.
    pub cause: Option<String>,
}

/// Where a rejected call goes: the rest of the candidate list, in the order
/// the call would have tried them.
pub struct Retry {
    /// How the call was written, for the diagnostic.
    pub name: Option<String>,
    pub rest: Vec<Value>,
    pub args: crate::vm::CallArgs,
    pub rejected: Vec<RejectedBy>,
}

impl Retry {
    /// What a call leaves behind in case the callee rejects: the candidates
    /// it has not tried, and the arguments to try them with. Kept even when
    /// there are none left, because the crash still needs to say what the call
    /// was and what the one candidate said about it.
    ///
    /// Where there *is* something left, the arguments are marked shared on the
    /// way in. Holding a handle is free — that is what copy-on-write is for —
    /// but the next candidate has to be handed what the caller wrote, not what
    /// the one that rejected did to it (§5.1). Marking them is what makes a
    /// write in the rejecting candidate split its own copy.
    pub fn at(name: &str, rest: Vec<Value>, args: crate::vm::CallArgs) -> Retry {
        let args = if rest.is_empty() { args } else { args.retained() };
        Retry { name: Some(name.to_string()), rest, args, rejected: Vec::new() }
    }
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
    /// Each child's cancel flag, in the same order. The block holds them
    /// because a trail that is *running* has been taken out of the task table,
    /// and cancelling it must not depend on catching it at rest (§9.4).
    pub flags: Vec<Arc<CancelFlag>>,
    pub pending: usize,
    /// Set when a `race` has been decided by its first completion.
    pub decided: bool,

    // --- auto-channels (spec/hydra_channels.md) -----------------------------
    /// How many trails the block will have, where that is known before it runs
    /// — which is the row form always, and the others never. It is what makes
    /// an index that cannot exist a crash instead of a wait (channels §6.7).
    pub arity: Option<usize>,
    /// Set when the owner reaches its `JoinBlock`: no further trail can appear,
    /// so an index past the last one is now certainly wrong and a peer that has
    /// not started never will.
    pub spawn_done: bool,
    /// Channel indices whose trail has ended.
    pub finished: Vec<usize>,
    /// Values in flight: buffered by `:detach` and `:broadcast`, and offered by
    /// a `:wait` whose sender is parked behind them. FIFO, so messages between
    /// one pair arrive in the order they were sent (channels §5).
    pub mail: VecDeque<Msg>,
    /// Trails parked in `send` or `receive`.
    pub waiting: Vec<Waiter>,
}

/// One value on its way to a sibling.
pub struct Msg {
    pub from: usize,
    /// The destinations it was addressed to, or `None` for "any", which is
    /// every trail but the sender.
    pub to: Option<Vec<usize>>,
    pub value: Value,
    /// The sender, when it is parked until someone takes this (mode `:wait`).
    pub waiter: Option<TaskId>,
}

/// A trail parked on a channel call.
pub struct Waiter {
    pub task: TaskId,
    pub channel: usize,
    /// A parked `receive`: the sources it will take from, or `None` for any.
    /// A parked `send` is not here — its value waits in `mail` instead.
    pub from: Option<Vec<usize>>,
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

    pub fn is_finished(&self, channel: usize) -> bool {
        self.finished.contains(&channel)
    }

    /// Whether any trail other than `me` could still take part — either one
    /// that has not ended, or one that has not been spawned yet. `only`
    /// restricts the question to the indices a call named.
    pub fn any_peer_left(&self, me: usize, only: Option<&Vec<usize>>) -> bool {
        match only {
            Some(indices) => indices.iter().any(|i| !self.is_finished(*i)),
            None => {
                !self.spawn_done
                    || (0..self.children.len()).any(|i| i != me && !self.is_finished(i))
            }
        }
    }

    /// Whether a message addressed this way reaches `me`.
    pub fn addressed_to(msg: &Msg, me: usize) -> bool {
        match &msg.to {
            Some(indices) => indices.contains(&me),
            None => msg.from != me,
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
    /// This trail's own index within its block, which is what `channel()`
    /// answers and what a sibling addresses (channels §1).
    pub channel: Option<usize>,
    /// What a channel call answered while this task was parked, waiting to be
    /// pushed when it runs again.
    pub delivery: Option<Vec<Value>>,
    /// What the last call answered beyond its first value, waiting for the
    /// binding site that names them. Written by every return, read by the one
    /// instruction that spreads them (channels §6.2).
    pub extras: Vec<Value>,
    /// What the candidates of the last call that ran out of them said, while
    /// the `:reject` it answered is still on its way somewhere. Diagnostic
    /// only: printed if that `:reject` reaches a statement with nothing to hand
    /// it to (§8.1), and dropped as soon as anything consumes a value.
    pub rejection: Option<String>,
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
