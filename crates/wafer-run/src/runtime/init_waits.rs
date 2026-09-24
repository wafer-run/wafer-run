//! Runtime-wide wait-for graph of in-flight block inits, for init-cycle
//! detection.
//!
//! A block's [`BlockSlot`](crate::runtime::slot::BlockSlot) holds its lock
//! across `lifecycle(Init)`. Code running inside one block's Init that needs
//! another block initialised waits on that block's slot lock, so an init
//! cycle (A's Init calls B, B's Init calls A) deadlocks unless something
//! refuses the wait that closes it. Whether the two inits run inside one
//! dispatch (A's Init reaches B's Init inline) or in two concurrent ones
//! (each holds one slot lock), the shape is the same, so one graph shared by
//! the whole runtime sees both.
//!
//! - Every `lifecycle(Init)` run is an [`InitAttempt`]. While it runs, the
//!   graph records it as the *owner* of its block.
//! - Every context produced for code running on behalf of an attempt (its
//!   Init context, and every `call_block` sub-context derived from it)
//!   carries that attempt. When such code needs another block's init, the
//!   attempt *waits for* that block until the nested init returns.
//! - Before adding a wait edge, [`InitWaits::wait_for`] follows owner →
//!   the blocks its attempt waits for → their owners → … If the walk comes
//!   back to the waiting attempt, the wait would close a cycle, and it is
//!   refused with the cycle's path instead of being made.
//!
//! The refusal comes from the edge that closes the cycle: every attempt in a
//! deadlock is waiting, and each wait is checked against the owners and
//! waits recorded when it is made, so the last edge of any cycle always
//! sees the rest of it. Code that runs outside every Init (top-level
//! dispatch) holds no slot lock and adds no edge, so concurrent callers of
//! one block simply queue on its lock.
//!
//! The critical sections are a few map operations with no `.await`, so the
//! lock is a sync `parking_lot::Mutex`, and the guards' `Drop` needs no async
//! runtime (the `wasm32-unknown-unknown` workers build has no `tokio/rt`).
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §3, §4

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use parking_lot::Mutex;

/// One run of a block's `lifecycle(Init)`.
#[derive(Debug, Clone)]
pub(crate) struct InitAttempt {
    id: u64,
    block: Arc<str>,
}

/// The owners and waits of every in-flight init.
#[derive(Debug, Default)]
struct WaitGraph {
    /// Block name → the attempt running its `lifecycle(Init)` now.
    owners: HashMap<String, u64>,
    /// Attempt → the blocks it is waiting on. A list, not a set: one Init
    /// can make concurrent calls, each holding its own edge.
    waits: HashMap<u64, Vec<String>>,
}

impl WaitGraph {
    /// Whether `goal` is reachable from `block`'s owner through the recorded
    /// waits. On `true`, `path` has been extended with the blocks walked
    /// after `block`, ending at `goal`'s block.
    fn reaches(
        &self,
        block: &str,
        goal: u64,
        path: &mut Vec<String>,
        seen: &mut HashSet<u64>,
    ) -> bool {
        let Some(&owner) = self.owners.get(block) else {
            return false;
        };
        if owner == goal {
            return true;
        }
        if !seen.insert(owner) {
            return false;
        }
        for next in self.waits.get(&owner).into_iter().flatten() {
            path.push(next.clone());
            if self.reaches(next, goal, path, seen) {
                return true;
            }
            path.pop();
        }
        false
    }
}

/// The runtime-wide wait-for graph. One per [`Wafer`](crate::Wafer), shared
/// by `Arc` with every context it produces.
#[derive(Debug, Default)]
pub(crate) struct InitWaits {
    graph: Mutex<WaitGraph>,
    next_attempt: AtomicU64,
}

impl InitWaits {
    /// A new attempt at `block`'s init. It becomes the block's owner only
    /// when it starts running ([`own`](Self::own)).
    pub(crate) fn attempt(&self, block: &str) -> InitAttempt {
        InitAttempt {
            id: self.next_attempt.fetch_add(1, Ordering::Relaxed),
            block: Arc::from(block),
        }
    }

    /// Record `attempt` as running its block's init, until the guard drops.
    /// Called with the block's slot lock held, so there is one owner per
    /// block at a time.
    pub(crate) fn own(self: &Arc<Self>, attempt: &InitAttempt) -> OwnerGuard {
        self.graph
            .lock()
            .owners
            .insert(attempt.block.to_string(), attempt.id);
        OwnerGuard {
            waits: self.clone(),
            attempt: attempt.clone(),
        }
    }

    /// Record that `waiter` waits for `target`'s init, until the guard
    /// drops — or refuse, with the cycle's path (`[waiter's block, target,
    /// …, waiter's block]`), when the wait would close an init cycle.
    pub(crate) fn wait_for(
        self: &Arc<Self>,
        waiter: &InitAttempt,
        target: &str,
    ) -> Result<WaitGuard, Vec<String>> {
        let mut graph = self.graph.lock();
        let mut path = vec![waiter.block.to_string(), target.to_string()];
        // On a hit, the walk ends at the block `waiter` owns, so `path`
        // already closes the cycle.
        if graph.reaches(target, waiter.id, &mut path, &mut HashSet::new()) {
            return Err(path);
        }
        graph
            .waits
            .entry(waiter.id)
            .or_default()
            .push(target.to_string());
        drop(graph);
        Ok(WaitGuard {
            waits: self.clone(),
            waiter: waiter.id,
            target: target.to_string(),
        })
    }
}

/// Removes an attempt's ownership of its block on drop.
#[derive(Debug)]
pub(crate) struct OwnerGuard {
    waits: Arc<InitWaits>,
    attempt: InitAttempt,
}

impl Drop for OwnerGuard {
    fn drop(&mut self) {
        let mut graph = self.waits.graph.lock();
        if graph.owners.get(&*self.attempt.block) == Some(&self.attempt.id) {
            graph.owners.remove(&*self.attempt.block);
        }
    }
}

/// Removes one wait edge on drop.
#[derive(Debug)]
pub(crate) struct WaitGuard {
    waits: Arc<InitWaits>,
    waiter: u64,
    target: String,
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        let mut graph = self.waits.graph.lock();
        if let Some(targets) = graph.waits.get_mut(&self.waiter) {
            if let Some(pos) = targets.iter().position(|t| *t == self.target) {
                targets.swap_remove(pos);
            }
            if targets.is_empty() {
                graph.waits.remove(&self.waiter);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_on_a_block_nobody_owns_is_allowed() {
        let waits = Arc::new(InitWaits::default());
        let a = waits.attempt("t/a");
        let _own_a = waits.own(&a);
        assert!(waits.wait_for(&a, "t/b").is_ok());
    }

    #[test]
    fn waiting_on_the_own_block_is_a_cycle() {
        let waits = Arc::new(InitWaits::default());
        let a = waits.attempt("t/a");
        let _own_a = waits.own(&a);
        assert_eq!(
            waits.wait_for(&a, "t/a").unwrap_err(),
            vec!["t/a".to_string(), "t/a".to_string()],
        );
    }

    #[test]
    fn two_attempts_waiting_on_each_other_close_a_cycle() {
        let waits = Arc::new(InitWaits::default());
        let a = waits.attempt("t/a");
        let b = waits.attempt("t/b");
        let _own_a = waits.own(&a);
        let _own_b = waits.own(&b);
        let _a_waits_b = waits.wait_for(&a, "t/b").expect("no cycle yet");
        assert_eq!(
            waits.wait_for(&b, "t/a").unwrap_err(),
            vec!["t/b".to_string(), "t/a".to_string(), "t/b".to_string()],
        );
    }

    #[test]
    fn a_longer_cycle_reports_every_block() {
        let waits = Arc::new(InitWaits::default());
        let a = waits.attempt("t/a");
        let b = waits.attempt("t/b");
        let c = waits.attempt("t/c");
        let _owners = [waits.own(&a), waits.own(&b), waits.own(&c)];
        let _ab = waits.wait_for(&a, "t/b").expect("no cycle yet");
        let _bc = waits.wait_for(&b, "t/c").expect("no cycle yet");
        assert_eq!(
            waits.wait_for(&c, "t/a").unwrap_err(),
            ["t/c", "t/a", "t/b", "t/c"].map(String::from).to_vec(),
        );
    }

    #[test]
    fn dropped_edges_and_owners_no_longer_count() {
        let waits = Arc::new(InitWaits::default());
        let a = waits.attempt("t/a");
        let b = waits.attempt("t/b");
        let own_a = waits.own(&a);
        let _own_b = waits.own(&b);
        let a_waits_b = waits.wait_for(&a, "t/b").expect("no cycle yet");
        drop(a_waits_b);
        assert!(waits.wait_for(&b, "t/a").is_ok(), "a no longer waits on b");

        drop(own_a);
        let a2 = waits.attempt("t/a");
        assert!(
            waits.wait_for(&a2, "t/a").is_ok(),
            "t/a has no owner once its attempt ended"
        );
    }

    #[test]
    fn concurrent_waits_from_one_attempt_are_counted_separately() {
        let waits = Arc::new(InitWaits::default());
        let a = waits.attempt("t/a");
        let b = waits.attempt("t/b");
        let _own_a = waits.own(&a);
        let _own_b = waits.own(&b);
        let first = waits.wait_for(&a, "t/b").expect("no cycle");
        let _second = waits
            .wait_for(&a, "t/b")
            .expect("a sibling wait is no cycle");
        drop(first);
        assert!(
            waits.wait_for(&b, "t/a").is_err(),
            "the second edge still stands"
        );
    }
}
