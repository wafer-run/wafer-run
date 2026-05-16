//! Per-dispatch init-stack for cycle detection.
//!
//! Carried through `RuntimeContext` (one stack per top-level dispatch).
//! When `Wafer::run` is called from outside, a fresh stack is created.
//! Nested dispatch calls (e.g., a block's lifecycle(Init) calls
//! wafer.run on another block) inherit the same stack via the context.
//!
//! Cycle detection: if a block name is already on the stack when
//! `push(name)` is called, return the full path `[existing..., name]`
//! as the cycle.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §3, §4

use std::sync::Arc;

use tokio::sync::Mutex;

#[derive(Debug, Clone, Default)]
pub struct InitStack {
    inner: Arc<Mutex<Vec<String>>>,
}

impl InitStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a block name onto the stack. Returns:
    /// - `Ok(InitGuard)` if the name was not already present. The guard pops
    ///   the name on drop.
    /// - `Err(cycle_path)` if the name is already on the stack. The path is
    ///   `[existing..., name]` — exactly what to surface as `InitError::Cycle`.
    pub async fn push(&self, name: &str) -> Result<InitGuard, Vec<String>> {
        let mut guard = self.inner.lock().await;
        if guard.iter().any(|n| n == name) {
            let mut path = guard.clone();
            path.push(name.to_string());
            return Err(path);
        }
        guard.push(name.to_string());
        Ok(InitGuard {
            inner: self.inner.clone(),
            name: name.to_string(),
        })
    }

    pub async fn snapshot(&self) -> Vec<String> {
        self.inner.lock().await.clone()
    }
}

/// RAII guard that pops the block name from the stack on drop.
#[derive(Debug)]
pub struct InitGuard {
    inner: Arc<Mutex<Vec<String>>>,
    name: String,
}

impl Drop for InitGuard {
    fn drop(&mut self) {
        let inner = self.inner.clone();
        let name = self.name.clone();
        // Drop is sync; we must briefly block to pop. The mutex is per-dispatch
        // and should never be contended at drop time (the same task that
        // pushed is also dropping the guard).
        let popped = {
            if let Ok(mut g) = inner.try_lock() {
                if let Some(pos) = g.iter().rposition(|n| n == &name) {
                    g.remove(pos);
                }
                true
            } else {
                false
            }
        };
        if !popped {
            // Best-effort: spawn a tokio task to pop. This branch should be
            // unreachable in normal flow because push and drop happen on the
            // same task without yielding the mutex.
            tokio::spawn(async move {
                let mut g = inner.lock().await;
                if let Some(pos) = g.iter().rposition(|n| n == &name) {
                    g.remove(pos);
                }
            });
        }
    }
}
