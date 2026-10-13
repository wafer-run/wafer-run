//! Platform-aware task spawning.
//!
//! `spawn_producer` runs a future concurrently:
//! - Native: `tokio::spawn` (requires tokio `rt` feature)
//! - wasm32 browser (wasm32-unknown-unknown): `wasm_bindgen_futures::spawn_local`,
//!   after passing the task through the thread's spawn wrapper (`set_spawn_wrapper`,
//!   below), if one is installed
//! - wasm32-wasip1: not supported — WASI blocks are synchronous; panics if called
//!
//! ## The spawn wrapper (wasm32-unknown-unknown)
//!
//! A wasm32 adapter may scope state to the request whose future is being
//! polled — a thread-local it installs on every poll of the request's
//! dispatch future and restores after it. A task spawned with `spawn_local`
//! is polled from the JS microtask queue instead, never inside that poll, so
//! it would see none of that state. `set_spawn_wrapper` (wasm32-unknown-unknown
//! only) lets the adapter close the gap: `spawn_producer` hands every task to
//! the installed wrapper **synchronously, on the spawning call's stack**,
//! before spawning what the wrapper returns. When the spawn happens inside a
//! poll — the usual case: a block's `handle` or a service call, inside the
//! request's dispatch future — the wrapper can capture what that poll made
//! current and re-enter it on every poll of the task. A wrapper must also
//! accept a spawn made outside any poll (init code, a JS callback, a `Drop`),
//! where nothing is current; it then passes the task through unchanged. A
//! task spawned from inside a wrapped task's poll is wrapped the same way, so
//! the state carries through nested spawns.
//!
//! The wrapper applies to every wasm32 [`spawn_producer`] call: the producer
//! task of [`OutputStream::from_producer`](crate::OutputStream::from_producer)
//! and any direct caller of the re-exported `spawn_producer`, such as a
//! deferred-task runner that spawns its tasks through it (impresspress's
//! `deferred::defer` in `Spawn` mode; nobody runs that on wasm32 today).
//! Those tasks are wrapped too, and capture what was current when they were
//! deferred. A raw `wasm_bindgen_futures::spawn_local` bypasses the wrapper.
//!
//! **A spawned task must finish its work before its stream's terminal.** Once
//! the consumer has read the terminal, nothing on the host keeps a request
//! alive for the task: on Cloudflare Workers its next I/O is refused or never
//! resolves, and the task is never polled again, holding what it captured.
//!
//! Native and wasm32-wasip1 have no wrapper: native services are not scoped
//! to a request, and `tokio::spawn` needs `Send`.

use std::future::Future;
#[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
use std::{cell::Cell, pin::Pin};

/// Spawn a fire-and-forget producer task.
///
/// On native targets this delegates to `tokio::spawn`.
/// On wasm32 browser targets this delegates to `wasm_bindgen_futures::spawn_local`,
/// through the installed spawn wrapper if there is one (see the
/// [module docs](self)).
/// On wasm32-wasip1 (WASI) targets this panics — WASI blocks must not use
/// streaming `OutputStream::from_producer`.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_producer<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(future);
}

/// A task [`spawn_producer`] is about to spawn on wasm32-unknown-unknown.
#[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
pub type SpawnedTask = Pin<Box<dyn Future<Output = ()> + 'static>>;

/// Wraps each task [`spawn_producer`] spawns, at the moment of spawning. See
/// the [module docs](self).
///
/// A wrapper returns the task to spawn; it must not spawn it itself through
/// `spawn_producer`, which would hand it back to the wrapper and recurse.
#[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
pub type SpawnWrapper = fn(SpawnedTask) -> SpawnedTask;

#[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
thread_local! {
    static WRAPPER: Cell<Option<SpawnWrapper>> = const { Cell::new(None) };
}

/// Install this thread's spawn wrapper, or remove it with `None`. Returns
/// the wrapper it replaces, so a caller that installs one temporarily can
/// put the previous one back. See the [module docs](self).
#[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
pub fn set_spawn_wrapper(wrapper: Option<SpawnWrapper>) -> Option<SpawnWrapper> {
    WRAPPER.with(|slot| slot.replace(wrapper))
}

/// Browser WASM (wasm32-unknown-unknown): pass the task through the
/// installed [spawn wrapper](set_spawn_wrapper), if any, then delegate to
/// wasm_bindgen_futures.
#[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
pub fn spawn_producer<F>(future: F)
where
    F: Future<Output = ()> + 'static,
{
    let task: SpawnedTask = Box::pin(future);
    let task = match WRAPPER.with(Cell::get) {
        Some(wrap) => wrap(task),
        None => task,
    };
    wasm_bindgen_futures::spawn_local(task);
}

/// WASI blocks (wasm32-wasip1) are synchronous — `from_producer` must not be called.
#[cfg(all(target_arch = "wasm32", target_os = "wasi"))]
pub fn spawn_producer<F>(_future: F)
where
    F: Future<Output = ()> + 'static,
{
    panic!("spawn_producer is not supported on wasm32-wasip1 (WASI); blocks must return GuestResult directly");
}
