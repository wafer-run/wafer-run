//! The wasm32 spawn wrapper (`wafer_block::spawn::set_spawn_wrapper`).
//!
//! These run under `wasm-bindgen-test-runner` on Node, because the wrapper
//! exists only on wasm32-unknown-unknown and `spawn_local` needs a JS
//! microtask queue: `scripts/check.sh wasm` runs them, as CI does.
//!
//! The "request scope" here is a thread-local marker that a wrapper captures
//! at spawn time and re-installs around every poll of the task — the shape
//! e.g. Cloudflare's adapter gives its own request-current state. Every
//! stream is read from outside any marker, so a producer that sees one can
//! only have been given it by the wrapper.
#![cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]

use std::{
    cell::Cell,
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};

use futures::StreamExt;
use wafer_block::{
    spawn::{set_spawn_wrapper, SpawnWrapper, SpawnedTask},
    OutputStream, StreamEvent,
};
use wasm_bindgen_test::wasm_bindgen_test;

thread_local! {
    /// The "request" whose scope is current, if any.
    static MARKER: Cell<Option<u32>> = const { Cell::new(None) };
    /// How many tasks [`count_spawns`] has wrapped.
    static COUNTED: Cell<usize> = const { Cell::new(0) };
}

fn marker() -> Option<u32> {
    MARKER.with(Cell::get)
}

/// Run `f` with `value` as the current marker, restoring the previous one.
fn with_marker<T>(value: u32, f: impl FnOnce() -> T) -> T {
    let previous = MARKER.with(|m| m.replace(Some(value)));
    let out = f();
    MARKER.with(|m| m.set(previous));
    out
}

/// Re-installs the marker it captured on every poll of `inner`.
struct Rescoped {
    marker: Option<u32>,
    inner: SpawnedTask,
}

impl Future for Rescoped {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let previous = MARKER.with(|m| m.replace(self.marker));
        let out = self.inner.as_mut().poll(cx);
        MARKER.with(|m| m.set(previous));
        out
    }
}

/// The adapter-shaped wrapper: capture the marker current at spawn time.
fn capture_marker(task: SpawnedTask) -> SpawnedTask {
    Box::pin(Rescoped {
        marker: marker(),
        inner: task,
    })
}

/// A wrapper that only counts the tasks it is handed.
fn count_spawns(task: SpawnedTask) -> SpawnedTask {
    COUNTED.with(|n| n.set(n.get() + 1));
    task
}

/// Installs a wrapper for one test and puts the previous one back on drop,
/// so no test leaves the thread's wrapper changed for the next.
struct WrapperGuard(Option<SpawnWrapper>);

impl WrapperGuard {
    fn install(wrapper: Option<SpawnWrapper>) -> Self {
        Self(set_spawn_wrapper(wrapper))
    }
}

impl Drop for WrapperGuard {
    fn drop(&mut self) {
        set_spawn_wrapper(self.0.take());
    }
}

/// Pending once, waking itself: an await point that sends the task back to
/// the microtask queue, so the next poll is a fresh one from outside.
struct YieldNow(bool);

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            return Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

fn describe(marker: Option<u32>) -> String {
    marker.map_or_else(|| "none".to_string(), |m| m.to_string())
}

/// A producer that reports the marker it sees on its first poll and after
/// an await, as one chunk `"<first>,<after>"`.
fn reporting_producer() -> OutputStream {
    OutputStream::from_producer(|sink, _cancel| async move {
        let first = marker();
        YieldNow(false).await;
        let after = marker();
        let _ = sink
            .send_chunk(format!("{},{}", describe(first), describe(after)).into_bytes())
            .await;
        let _ = sink.complete(Vec::new()).await;
    })
}

/// Every chunk of `stream`, read from outside any marker, joined.
async fn body(stream: OutputStream) -> String {
    assert_eq!(marker(), None, "the reader runs outside every marker");
    read_all(stream).await
}

/// Every chunk of `stream`, joined, read wherever the caller is.
async fn read_all(mut stream: OutputStream) -> String {
    let mut out = Vec::new();
    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::Chunk(bytes) => out.extend(bytes),
            StreamEvent::Complete { .. } => break,
            other => panic!("unexpected event {other:?}"),
        }
    }
    String::from_utf8(out).expect("utf-8 body")
}

#[wasm_bindgen_test]
async fn a_wrapper_sees_what_was_current_when_the_producer_was_spawned() {
    let _guard = WrapperGuard::install(Some(capture_marker));
    let stream = with_marker(7, reporting_producer);
    assert_eq!(marker(), None);
    assert_eq!(
        body(stream).await,
        "7,7",
        "the producer runs under the marker of its spawn, on every poll"
    );
}

#[wasm_bindgen_test]
async fn without_a_wrapper_a_producer_runs_bare() {
    let _guard = WrapperGuard::install(None);
    let stream = with_marker(7, reporting_producer);
    assert_eq!(
        body(stream).await,
        "none,none",
        "a bare spawn_local task is never polled inside its spawner's poll"
    );
}

/// The wrapper runs on the spawning stack, so a producer spawned during a
/// wrapped producer's poll captures the marker that poll re-entered: the
/// shape of a streaming block reached by `call_block` from a producer.
#[wasm_bindgen_test]
async fn a_producer_spawned_from_inside_a_wrapped_producer_is_wrapped() {
    let _guard = WrapperGuard::install(Some(capture_marker));
    let outer = with_marker(7, || {
        OutputStream::from_producer(|sink, _cancel| async move {
            YieldNow(false).await;
            // Spawned from a poll the wrapper re-entered, not from `with_marker`.
            let inner = reporting_producer();
            let _ = sink.send_chunk(read_all(inner).await.into_bytes()).await;
            let _ = sink.complete(Vec::new()).await;
        })
    });
    assert_eq!(body(outer).await, "7,7");
}

#[wasm_bindgen_test]
async fn set_spawn_wrapper_returns_the_one_it_replaced() {
    let _guard = WrapperGuard::install(None);

    assert!(
        set_spawn_wrapper(Some(count_spawns)).is_none(),
        "nothing was installed"
    );
    let counting = set_spawn_wrapper(Some(capture_marker));
    assert!(counting.is_some(), "the counting wrapper is handed back");

    // Putting back what was handed back reinstates it.
    set_spawn_wrapper(counting);
    let before = COUNTED.with(Cell::get);
    assert_eq!(body(reporting_producer()).await, "none,none");
    assert_eq!(
        COUNTED.with(Cell::get),
        before + 1,
        "the reinstated wrapper wrapped the spawn"
    );
}

#[wasm_bindgen_test]
async fn dropping_the_stream_still_cancels_a_wrapped_producer() {
    let _guard = WrapperGuard::install(Some(capture_marker));
    let (tx, rx) = futures::channel::oneshot::channel::<Option<u32>>();
    let first_chunk = Rc::new(Cell::new(false));
    let sent = Rc::clone(&first_chunk);
    let mut stream = with_marker(7, || {
        OutputStream::from_producer(move |sink, cancel| async move {
            let _ = sink.send_chunk(b"one".to_vec()).await;
            sent.set(true);
            cancel.cancelled().await;
            let _ = tx.send(marker());
        })
    });
    assert!(matches!(stream.next().await, Some(StreamEvent::Chunk(_))));
    drop(stream);
    assert_eq!(
        rx.await.expect("the producer saw its cancellation"),
        Some(7),
        "cancelled, and still in its scope when it woke"
    );
    assert!(first_chunk.get());
}
