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
//! ## Why a sync (`parking_lot::Mutex`) rather than `tokio::sync::Mutex`
//!
//! The critical sections here are tiny (a membership scan + push/clone of
//! a small `Vec<String>`) with no `.await` while the lock is held, so
//! blocking briefly is fine and preferable to pulling in tokio's
//! async-mutex machinery. The decisive reason for the switch was that
//! `Drop for InitGuard` is sync and used to fall back to `tokio::spawn`
//! when `try_lock` failed; that hard-required `tokio/rt`, which is not
//! available in `wasm32-unknown-unknown` workers (solobase-cloudflare).
//! With a sync mutex `Drop` is straightforward and the tokio-rt
//! dependency vanishes from the lock path.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §3, §4

use std::sync::Arc;

use parking_lot::Mutex;

/// Shared per-dispatch stack of block names currently being initialised, used to detect cycles.
#[derive(Debug, Clone, Default)]
pub struct InitStack {
    inner: Arc<Mutex<Vec<String>>>,
}

impl InitStack {
    /// Build an empty stack.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a block name onto the stack. Returns:
    /// - `Ok(InitGuard)` if the name was not already present. The guard pops
    ///   the name on drop.
    /// - `Err(cycle_path)` if the name is already on the stack. The path is
    ///   `[existing..., name]` — exactly what to surface as `InitError::Cycle`.
    pub fn push(&self, name: &str) -> Result<InitGuard, Vec<String>> {
        let mut guard = self.inner.lock();
        if guard.iter().any(|n| n == name) {
            let mut path = guard.clone();
            path.push(name.to_string());
            return Err(path);
        }
        guard.push(name.to_string());
        drop(guard);
        Ok(InitGuard {
            inner: self.inner.clone(),
            name: name.to_string(),
        })
    }

    /// Clone-snapshot the current stack contents for diagnostics.
    pub fn snapshot(&self) -> Vec<String> {
        self.inner.lock().clone()
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
        // `parking_lot::Mutex::lock` is sync and infallible. No
        // `tokio::spawn` fallback needed, so this path no longer requires
        // `tokio/rt` and compiles cleanly for `wasm32-unknown-unknown`.
        let mut g = self.inner.lock();
        if let Some(pos) = g.iter().rposition(|n| n == &self.name) {
            g.remove(pos);
        }
    }
}
