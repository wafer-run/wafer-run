# Streaming Protocol Ergonomics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add three ergonomic helpers (`OutputStream::from_producer`, `OutputStream::from_result`, `Context::call_block_buffered`) to the streaming protocol, add `OutputSink` auto-complete on Drop, add `body_stream_or_error`, and update the design spec with fixes for review issues.

**Architecture:** All code changes are in the `wafer-block` crate (`wafer-run/crates/wafer-block`). A new `spawn` module provides platform-aware task spawning (`tokio::spawn` on native, `wasm_bindgen_futures::spawn_local` on wasm32). `OutputSink` gains a Drop impl that auto-emits `Complete` if no terminal was sent. `Context` gains a `call_block_buffered` default method. The spec document at `wafer-run/docs/specs/2026-04-15-streaming-protocol-design.md` is updated with all fixes.

**Tech Stack:** Rust, tokio (sync + rt), tokio-util, futures, wasm-bindgen-futures (wasm32 target only)

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `crates/wafer-block/Cargo.toml` | Modify | Add target-specific deps for spawn |
| `crates/wafer-block/src/spawn.rs` | Create | Platform-aware `spawn_producer` function |
| `crates/wafer-block/src/lib.rs` | Modify | Add `spawn` module, re-export `spawn_producer` |
| `crates/wafer-block/src/streams/output.rs` | Modify | Auto-complete Drop on OutputSink, `from_producer`, `from_result`, `body_stream_or_error` |
| `crates/wafer-block/src/context.rs` | Modify | Add `call_block_buffered` default method |
| `docs/specs/2026-04-15-streaming-protocol-design.md` | Modify | Fix review issues, add new helpers |

---

### Task 1: Add `spawn` module with platform-aware task spawning

**Files:**
- Modify: `crates/wafer-block/Cargo.toml`
- Create: `crates/wafer-block/src/spawn.rs`
- Modify: `crates/wafer-block/src/lib.rs`

- [ ] **Step 1: Add target-specific dependencies to Cargo.toml**

In `crates/wafer-block/Cargo.toml`, add after the existing `tokio-stream` line (before `[dev-dependencies]`):

```toml
# Platform-aware spawn for OutputStream::from_producer.
# Native: tokio/rt for tokio::spawn. WASM: wasm-bindgen-futures for spawn_local.
[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
tokio = { version = "1", default-features = false, features = ["rt"] }

[target.'cfg(target_arch = "wasm32")'.dependencies]
wasm-bindgen-futures = "0.4"
```

This merges with the existing `tokio` dep — native builds get `sync` + `rt`, wasm32 builds get `sync` only.

- [ ] **Step 2: Create the spawn module**

Create `crates/wafer-block/src/spawn.rs`:

```rust
//! Platform-aware task spawning.
//!
//! `spawn_producer` runs a future concurrently:
//! - Native: `tokio::spawn` (requires tokio `rt` feature)
//! - wasm32: `wasm_bindgen_futures::spawn_local`

use std::future::Future;

/// Spawn a fire-and-forget producer task.
///
/// On native targets this delegates to `tokio::spawn`.
/// On wasm32 this delegates to `wasm_bindgen_futures::spawn_local`.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_producer<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(future);
}

#[cfg(target_arch = "wasm32")]
pub fn spawn_producer<F>(future: F)
where
    F: Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(future);
}
```

- [ ] **Step 3: Register the module and re-export**

In `crates/wafer-block/src/lib.rs`, add `pub mod spawn;` after the existing `pub mod stream;` line (line 43). Then add a re-export after the existing re-exports at the bottom:

```rust
pub mod spawn;
```

And at the bottom with the other re-exports:

```rust
pub use spawn::spawn_producer;
```

- [ ] **Step 4: Verify it compiles**

Run: `cd wafer-run && cargo check -p wafer-block`
Expected: compiles with no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block/Cargo.toml crates/wafer-block/src/spawn.rs crates/wafer-block/src/lib.rs
git commit -m "feat(wafer-block): add platform-aware spawn_producer module"
```

---

### Task 2: Add auto-complete Drop on OutputSink

**Files:**
- Modify: `crates/wafer-block/src/streams/output.rs`

The `OutputSink` currently has no `Drop` impl. When dropped without calling a terminal method, the channel simply closes and the `OutputStream` consumer sees `Malformed`. We add a `terminal_sent` flag and a `Drop` impl that auto-emits `Complete { meta: vec![] }` via `try_send` when no terminal was explicitly sent.

- [ ] **Step 1: Write the failing test**

Add at the bottom of the `#[cfg(test)] mod tests` block in `crates/wafer-block/src/streams/output.rs`:

```rust
    #[tokio::test]
    async fn sink_auto_completes_on_drop() {
        let (stream, sink, _cancel) = OutputStream::new_streaming();
        // Send a chunk, then drop the sink without calling a terminal.
        sink.send_chunk(b"hello".to_vec()).await.unwrap();
        drop(sink);

        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], StreamEvent::Chunk(b"hello".to_vec()));
        assert!(
            matches!(events[1], StreamEvent::Complete { ref meta } if meta.is_empty()),
            "sink should auto-complete on drop, got: {:?}",
            events[1]
        );
    }

    #[tokio::test]
    async fn sink_does_not_double_complete() {
        let (stream, sink, _cancel) = OutputStream::new_streaming();
        sink.complete(vec![]).await.unwrap();
        // sink is dropped here — should NOT emit a second Complete.

        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 1, "should have exactly one terminal");
        assert!(matches!(events[0], StreamEvent::Complete { .. }));
    }

    #[tokio::test]
    async fn sink_does_not_auto_complete_after_error() {
        let (stream, sink, _cancel) = OutputStream::new_streaming();
        let err = crate::core_types::WaferError {
            code: crate::core_types::ErrorCode::Internal,
            message: "fail".into(),
            meta: vec![],
        };
        sink.error(err.clone()).await.unwrap();

        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], StreamEvent::Error(err));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd wafer-run && cargo test --lib -p wafer-block -- sink_auto_completes_on_drop sink_does_not_double_complete sink_does_not_auto_complete_after_error`
Expected: `sink_auto_completes_on_drop` FAILS (gets `Malformed` or 1 event instead of 2). The other two may pass or fail depending on timing.

- [ ] **Step 3: Add `terminal_sent` field and Drop impl to OutputSink**

In `crates/wafer-block/src/streams/output.rs`, modify the `OutputSink` struct:

```rust
pub struct OutputSink {
    tx: mpsc::Sender<StreamEvent>,
    terminal_sent: bool,
    #[cfg(debug_assertions)]
    any_body_sent: std::sync::atomic::AtomicBool,
}
```

Add the `Drop` impl right after the `OutputSink` impl block (after line 94):

```rust
impl Drop for OutputSink {
    fn drop(&mut self) {
        if !self.terminal_sent {
            // Auto-complete: best-effort send via try_send (sync, non-blocking).
            // If the channel is full or the consumer is gone, this silently fails.
            let _ = self.tx.try_send(StreamEvent::Complete { meta: vec![] });
        }
    }
}
```

Update each terminal method to set `terminal_sent = true`. Change `complete` to take `mut self`:

```rust
    pub async fn complete(mut self, meta: Vec<MetaEntry>) -> Result<(), SinkClosed> {
        self.terminal_sent = true;
        self.tx
            .send(StreamEvent::Complete { meta })
            .await
            .map_err(|_| SinkClosed)
    }

    pub async fn error(mut self, err: WaferError) -> Result<(), SinkClosed> {
        self.terminal_sent = true;
        self.tx
            .send(StreamEvent::Error(err))
            .await
            .map_err(|_| SinkClosed)
    }

    pub async fn drop_request(mut self) -> Result<(), SinkClosed> {
        #[cfg(debug_assertions)]
        assert!(
            !self
                .any_body_sent
                .load(std::sync::atomic::Ordering::Relaxed),
            "Drop terminal cannot follow Chunk or Meta events"
        );
        self.terminal_sent = true;
        self.tx
            .send(StreamEvent::Drop)
            .await
            .map_err(|_| SinkClosed)
    }

    pub async fn continue_with(mut self, msg: Message) -> Result<(), SinkClosed> {
        #[cfg(debug_assertions)]
        assert!(
            !self
                .any_body_sent
                .load(std::sync::atomic::Ordering::Relaxed),
            "Continue terminal cannot follow Chunk or Meta events"
        );
        self.terminal_sent = true;
        self.tx
            .send(StreamEvent::Continue(msg))
            .await
            .map_err(|_| SinkClosed)
    }
```

Update `new_streaming_channel` to initialize the new field:

```rust
pub(crate) fn new_streaming_channel(
    capacity: usize,
) -> (mpsc::Receiver<StreamEvent>, OutputSink, CancellationToken) {
    let (tx, rx) = mpsc::channel(capacity);
    let cancel = CancellationToken::new();
    let sink = OutputSink {
        tx,
        terminal_sent: false,
        #[cfg(debug_assertions)]
        any_body_sent: std::sync::atomic::AtomicBool::new(false),
    };
    (rx, sink, cancel)
}
```

- [ ] **Step 4: Run all output stream tests**

Run: `cd wafer-run && cargo test --lib -p wafer-block -- streams::output`
Expected: all tests pass (including the 3 new ones and all existing ones).

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block/src/streams/output.rs
git commit -m "feat(wafer-block): auto-complete OutputSink on Drop"
```

---

### Task 3: Add `OutputStream::from_producer`

**Files:**
- Modify: `crates/wafer-block/src/streams/output.rs`

- [ ] **Step 1: Write the failing tests**

Add to the test module in `crates/wafer-block/src/streams/output.rs`:

```rust
    #[tokio::test]
    async fn from_producer_streams_chunks_and_auto_completes() {
        let stream = OutputStream::from_producer(|sink, _cancel| async move {
            sink.send_chunk(b"a".to_vec()).await.ok();
            sink.send_chunk(b"b".to_vec()).await.ok();
            // No explicit terminal — auto-complete on drop.
        });
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 3);
        assert_eq!(events[0], StreamEvent::Chunk(b"a".to_vec()));
        assert_eq!(events[1], StreamEvent::Chunk(b"b".to_vec()));
        assert!(matches!(events[2], StreamEvent::Complete { ref meta } if meta.is_empty()));
    }

    #[tokio::test]
    async fn from_producer_explicit_error_terminal() {
        let stream = OutputStream::from_producer(|sink, _cancel| async move {
            sink.send_chunk(b"partial".to_vec()).await.ok();
            let _ = sink
                .error(crate::core_types::WaferError {
                    code: crate::core_types::ErrorCode::Internal,
                    message: "upstream failed".into(),
                    meta: vec![],
                })
                .await;
        });
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], StreamEvent::Chunk(b"partial".to_vec()));
        assert!(matches!(events[1], StreamEvent::Error(_)));
    }

    #[tokio::test]
    async fn from_producer_stops_on_consumer_drop() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let finished = Arc::new(AtomicBool::new(false));
        let finished_clone = finished.clone();
        let stream = OutputStream::from_producer(move |sink, _cancel| async move {
            for i in 0u8..255 {
                if sink.send_chunk(vec![i]).await.is_err() {
                    finished_clone.store(true, Ordering::SeqCst);
                    return;
                }
            }
        });
        // Read one chunk then drop.
        let mut stream = stream;
        use futures::StreamExt;
        let first = stream.next().await;
        assert!(first.is_some());
        drop(stream);

        // Give the producer task time to notice.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(finished.load(Ordering::SeqCst));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd wafer-run && cargo test --lib -p wafer-block -- from_producer`
Expected: compile error — `from_producer` method does not exist.

- [ ] **Step 3: Implement `from_producer`**

In `crates/wafer-block/src/streams/output.rs`, add these imports at the top:

```rust
use std::future::Future;
```

Then add the method inside the existing second `impl OutputStream` block (the one starting at line 236 that has `collect_buffered` and `body_stream`):

```rust
    /// Creates a streaming `OutputStream` driven by a producer closure.
    ///
    /// The closure receives an [`OutputSink`] and a [`CancellationToken`]. It should
    /// call `sink.send_chunk()` / `sink.send_meta()` for non-terminal events. When
    /// the closure returns, the sink is dropped — if no terminal was explicitly sent
    /// (via `sink.complete()`, `sink.error()`, etc.), an auto-`Complete { meta: vec![] }`
    /// is emitted.
    ///
    /// For explicit error handling, call `sink.error(e).await` before returning.
    ///
    /// Platform-portable: uses `tokio::spawn` on native, `spawn_local` on wasm32.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_producer<F, Fut>(f: F) -> Self
    where
        F: FnOnce(OutputSink, CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let (stream, sink, cancel) = Self::new_streaming();
        let cancel_clone = cancel.clone();
        crate::spawn::spawn_producer(async move {
            f(sink, cancel_clone).await;
        });
        stream
    }

    /// Creates a streaming `OutputStream` driven by a producer closure (wasm32 variant).
    #[cfg(target_arch = "wasm32")]
    pub fn from_producer<F, Fut>(f: F) -> Self
    where
        F: FnOnce(OutputSink, CancellationToken) -> Fut + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        let (stream, sink, cancel) = Self::new_streaming();
        let cancel_clone = cancel.clone();
        crate::spawn::spawn_producer(async move {
            f(sink, cancel_clone).await;
        });
        stream
    }
```

- [ ] **Step 4: Run tests**

Run: `cd wafer-run && cargo test --lib -p wafer-block -- from_producer`
Expected: all 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block/src/streams/output.rs
git commit -m "feat(wafer-block): add OutputStream::from_producer"
```

---

### Task 4: Add `OutputStream::from_result`

**Files:**
- Modify: `crates/wafer-block/src/streams/output.rs`

- [ ] **Step 1: Write the failing tests**

Add to the test module:

```rust
    #[tokio::test]
    async fn from_result_ok_is_respond() {
        let stream = OutputStream::from_result(Ok(b"data".to_vec()));
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], StreamEvent::Chunk(b"data".to_vec()));
        assert!(matches!(events[1], StreamEvent::Complete { .. }));
    }

    #[tokio::test]
    async fn from_result_err_is_error() {
        let err = crate::core_types::WaferError {
            code: crate::core_types::ErrorCode::NotFound,
            message: "gone".into(),
            meta: vec![],
        };
        let stream = OutputStream::from_result(Err::<Vec<u8>, _>(err.clone()));
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], StreamEvent::Error(err));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd wafer-run && cargo test --lib -p wafer-block -- from_result`
Expected: compile error — `from_result` does not exist.

- [ ] **Step 3: Implement `from_result`**

Add in the first `impl OutputStream` block (the one with `respond`, `error`, etc., starting around line 119), after `continue_with`:

```rust
    /// Convert a `Result<Vec<u8>, WaferError>` into an `OutputStream`.
    ///
    /// `Ok(bytes)` → `respond(bytes)`, `Err(e)` → `error(e)`.
    pub fn from_result(result: Result<Vec<u8>, WaferError>) -> Self {
        match result {
            Ok(bytes) => Self::respond(bytes),
            Err(e) => Self::error(e),
        }
    }
```

- [ ] **Step 4: Run tests**

Run: `cd wafer-run && cargo test --lib -p wafer-block -- from_result`
Expected: both tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block/src/streams/output.rs
git commit -m "feat(wafer-block): add OutputStream::from_result"
```

---

### Task 5: Add `OutputStream::body_stream_or_error`

**Files:**
- Modify: `crates/wafer-block/src/streams/output.rs`

- [ ] **Step 1: Write the failing tests**

Add to the test module:

```rust
    #[tokio::test]
    async fn body_stream_or_error_yields_chunks_then_ok() {
        let (stream, sink, _cancel) = OutputStream::new_streaming();
        tokio::spawn(async move {
            sink.send_chunk(b"a".to_vec()).await.unwrap();
            sink.send_chunk(b"b".to_vec()).await.unwrap();
            sink.complete(vec![]).await.unwrap();
        });
        let items: Vec<_> = stream.body_stream_or_error().collect().await;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].as_ref().unwrap(), &b"a".to_vec());
        assert_eq!(items[1].as_ref().unwrap(), &b"b".to_vec());
    }

    #[tokio::test]
    async fn body_stream_or_error_yields_err_on_error_terminal() {
        let (stream, sink, _cancel) = OutputStream::new_streaming();
        tokio::spawn(async move {
            sink.send_chunk(b"partial".to_vec()).await.unwrap();
            sink.error(crate::core_types::WaferError {
                code: crate::core_types::ErrorCode::Internal,
                message: "upstream".into(),
                meta: vec![],
            })
            .await
            .unwrap();
        });
        let items: Vec<Result<Vec<u8>, crate::core_types::WaferError>> =
            stream.body_stream_or_error().collect().await;
        assert_eq!(items.len(), 2);
        assert!(items[0].is_ok());
        assert!(items[1].is_err());
        assert_eq!(items[1].as_ref().unwrap_err().message, "upstream");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd wafer-run && cargo test --lib -p wafer-block -- body_stream_or_error`
Expected: compile error — method does not exist.

- [ ] **Step 3: Implement `body_stream_or_error`**

Add in the second `impl OutputStream` block, after `body_stream`:

```rust
    /// Like [`body_stream`](Self::body_stream) but propagates `Error` terminals
    /// as a final `Err` item instead of swallowing them.
    ///
    /// Yields `Ok(bytes)` for each `Chunk`, skips `Meta` events, and stops at
    /// the first terminal. If the terminal is `Error(e)`, it is yielded as the
    /// last item (`Err(e)`). `Complete`/`Drop`/`Continue` terminals end the
    /// stream without an error item.
    pub fn body_stream_or_error(
        self,
    ) -> impl Stream<Item = Result<Vec<u8>, WaferError>> + Send + 'static {
        use futures::StreamExt;
        self.filter_map(|evt| async move {
            match evt {
                StreamEvent::Chunk(bytes) => Some(Ok(bytes)),
                StreamEvent::Error(e) => Some(Err(e)),
                StreamEvent::Meta(_) => None,
                // Terminal non-error events end the stream.
                StreamEvent::Complete { .. } | StreamEvent::Drop | StreamEvent::Continue(_) => None,
            }
        })
    }
```

- [ ] **Step 4: Run tests**

Run: `cd wafer-run && cargo test --lib -p wafer-block -- body_stream_or_error`
Expected: both tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block/src/streams/output.rs
git commit -m "feat(wafer-block): add OutputStream::body_stream_or_error"
```

---

### Task 6: Add `Context::call_block_buffered` default method

**Files:**
- Modify: `crates/wafer-block/src/streams/output.rs` (add `impl From<TerminalNotResponse> for WaferError`)
- Modify: `crates/wafer-block/src/context.rs`

- [ ] **Step 1: Add `From<TerminalNotResponse> for WaferError`**

In `crates/wafer-block/src/streams/output.rs`, add after the `TerminalNotResponse` enum definition (after line 234):

```rust
impl From<TerminalNotResponse> for WaferError {
    fn from(t: TerminalNotResponse) -> Self {
        match t {
            TerminalNotResponse::Error(e) => e,
            TerminalNotResponse::Drop => WaferError {
                code: crate::core_types::ErrorCode::Unknown,
                message: "block dropped the request".into(),
                meta: vec![],
            },
            TerminalNotResponse::Continue(msg) => WaferError {
                code: crate::core_types::ErrorCode::Internal,
                message: format!("unexpected Continue terminal (kind: {})", msg.kind),
                meta: vec![],
            },
            TerminalNotResponse::Malformed => WaferError {
                code: crate::core_types::ErrorCode::Internal,
                message: "stream ended without terminal event (protocol violation)".into(),
                meta: vec![],
            },
        }
    }
}
```

- [ ] **Step 2: Add the default method to Context**

In `crates/wafer-block/src/context.rs`, add the necessary imports and the default method. Replace the entire file with:

```rust
//! The Context trait — runtime capabilities provided to blocks.

use crate::core_types::{Message, WaferError};
use crate::streams::input::InputStream;
use crate::streams::output::{BufferedResponse, OutputStream};
use crate::types::BlockInfo;

/// Context provides runtime capabilities to blocks.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait Context: crate::compat::MaybeSend + crate::compat::MaybeSync {
    /// Call another block by name.
    async fn call_block(&self, block_name: &str, msg: Message, input: InputStream) -> OutputStream;

    /// Call another block and collect the full buffered response.
    ///
    /// Convenience wrapper: builds an `InputStream` from `body`, calls the block,
    /// and drains the `OutputStream` into a `BufferedResponse`. Returns `Err` if
    /// the stream terminates with anything other than `Complete`.
    async fn call_block_buffered(
        &self,
        block_name: &str,
        msg: Message,
        body: &[u8],
    ) -> Result<BufferedResponse, WaferError> {
        let input = if body.is_empty() {
            InputStream::empty()
        } else {
            InputStream::from_bytes(body.to_vec())
        };
        let output = self.call_block(block_name, msg, input).await;
        output.collect_buffered().await.map_err(WaferError::from)
    }

    /// Check if the context has been cancelled.
    fn is_cancelled(&self) -> bool;

    /// Get a config value from the block's node config.
    fn config_get(&self, key: &str) -> Option<&str>;

    /// List all registered blocks.
    fn registered_blocks(&self) -> Vec<BlockInfo> {
        Vec::new()
    }

    /// List flow summary info.
    fn flow_infos(&self) -> Vec<wafer_flow::FlowInfo> {
        Vec::new()
    }

    /// List full flow definitions.
    fn flow_defs(&self) -> Vec<wafer_flow::WaferFlow> {
        Vec::new()
    }

    /// Get expanded block configs (for inspector app view).
    fn block_configs(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::new()
    }

    /// List registered interface specifications.
    fn interface_specs(&self) -> Vec<crate::types::InterfaceSpec> {
        Vec::new()
    }

    /// The block name of the caller that invoked this block via `call_block()`.
    /// Returns `None` for top-level calls (e.g. from the router).
    fn caller_id(&self) -> Option<&str> {
        None
    }
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cd wafer-run && cargo check -p wafer-block`
Expected: compiles. The `call_block_buffered` default method uses only types already available.

- [ ] **Step 4: Run all wafer-block tests**

Run: `cd wafer-run && cargo test --lib -p wafer-block`
Expected: all tests pass.

- [ ] **Step 5: Verify downstream crates still compile**

Run: `cd wafer-run && cargo check`
Expected: full workspace compiles. The new default method doesn't break existing `Context` impls since they inherit the default.

- [ ] **Step 6: Commit**

```bash
git add crates/wafer-block/src/streams/output.rs crates/wafer-block/src/context.rs
git commit -m "feat(wafer-block): add Context::call_block_buffered default method"
```

---

### Task 7: Update the design spec

**Files:**
- Modify: `docs/specs/2026-04-15-streaming-protocol-design.md`

This task updates the spec with the ergonomic helpers and fixes for all review issues.

- [ ] **Step 1: Add ergonomic helpers to Types section**

After the `BufferedResponse` struct definition (around line 153), add:

````markdown
### Ergonomic Helpers

**`OutputStream::from_producer`** — platform-aware spawn with auto-complete:

```rust
// Works on native AND wasm32. Closure returns () — if no terminal
// is sent on the sink, Complete { meta: vec![] } is auto-emitted on drop.
OutputStream::from_producer(|sink, cancel| async move {
    sink.send_meta(MetaEntry::content_type("text/event-stream")).await.ok();
    let mut upstream = service.stream(cancel).await.unwrap();
    while let Some(chunk) = upstream.next().await {
        if sink.send_chunk(chunk).await.is_err() { return; }
    }
    // auto-complete on drop
})
```

On native: wraps in `tokio::spawn`. On wasm32: wraps in `wasm_bindgen_futures::spawn_local`. The `spawn_producer` function in `wafer-block/src/spawn.rs` provides the platform abstraction.

**`OutputStream::from_result`** — one-liner for buffered blocks:

```rust
OutputStream::from_result(self.process(&msg, &body).await)
// Ok(bytes) → respond(bytes), Err(e) → error(e)
```

**`Context::call_block_buffered`** — shorthand for the 90% call pattern:

```rust
let response = ctx.call_block_buffered("wafer-run/database", msg, &body).await?;
// Returns Result<BufferedResponse, WaferError>
```

Default method on `Context`; builds `InputStream` from `&[u8]`, calls `call_block`, drains via `collect_buffered`, and converts `TerminalNotResponse` to `WaferError`.

**`OutputStream::body_stream_or_error`** — error-propagating body stream:

```rust
let chunks = prev_output.body_stream_or_error();
// Stream<Item = Result<Vec<u8>, WaferError>>
// Propagates Error terminals instead of swallowing them.
```

**`OutputSink` auto-complete on Drop:**

When an `OutputSink` is dropped without having called any terminal method (`complete`, `error`, `drop_request`, `continue_with`), the `Drop` impl auto-emits `Complete { meta: vec![] }` via `try_send`. This makes `from_producer` ergonomic: the closure simply returns and the stream completes. If a terminal was explicitly sent, the auto-complete is suppressed via an internal `terminal_sent` flag.
````

- [ ] **Step 2: Add Continue re-dispatch depth limit**

In the "Transports → HTTP listener" section (around line 319), after the description of Continue handling, add:

```markdown
   Continue re-dispatch is limited to a maximum depth of **8**. If the depth is exceeded, the adapter emits a 508 (Loop Detected) response with a JSON error body. This prevents infinite forwarding loops between blocks.
```

- [ ] **Step 3: Add InputStream cancellation semantics**

In the "Cancellation Semantics" section (around line 277), add a new paragraph:

```markdown
**Input-side cancellation:** When a block returns an `OutputStream` before fully consuming its `InputStream`, the runtime fires the `InputStream`'s `CancellationToken`. This signals the upstream producer (e.g., the HTTP adapter streaming a request body) to stop sending. For large uploads, this means a block that rejects on the first chunk causes the upload to abort rather than buffering the full body. Transport adapters are responsible for wiring the `InputStream` cancel token to their upstream source (e.g., dropping the axum `Body`).
```

- [ ] **Step 4: Add Meta ordering enforcement**

In the "Invariants" section, update invariant 4 (around line 161):

```markdown
4. `Meta` events should precede their semantic effect — e.g., a `Content-Type: text/event-stream` declaration should be emitted before any `Chunk`, because the HTTP adapter commits to SSE framing on seeing it. **Enforcement:** If the HTTP adapter sees a `Content-Type: text/event-stream` Meta after already receiving a `Chunk`, it logs a warning and continues in buffered mode — the late declaration is ignored. This is a runtime check, not a debug assertion, to prevent silent misbehavior in production.
```

- [ ] **Step 5: Fix `OutputStream::drop()` naming**

Search the spec for `OutputStream::drop()` and replace with `OutputStream::drop_request()`. The current spec text at line 99 already says `drop_request` — verify no other occurrences use the bare `drop()` name for the constructor.

- [ ] **Step 6: Add flow/pipeline integration note**

In the "Out of Scope (Explicit)" section (around line 511), add:

```markdown
- **Flow/pipeline streaming plumbing.** The declarative flow engine (`wafer-flow`) currently composes blocks via `call_block`. Flows inherit streaming support automatically through the updated `call_block` signature — each flow step's `OutputStream` can be piped to the next step's `InputStream` via `body_stream()` or consumed directly. No flow engine changes are needed for this spec. If flows later need streaming-aware routing logic (e.g., "route based on first chunk"), that's a separate evolution.
```

- [ ] **Step 7: Verify spec is self-consistent**

Read through the updated spec once to check no internal contradictions were introduced.

- [ ] **Step 8: Commit**

```bash
git add docs/specs/2026-04-15-streaming-protocol-design.md
git commit -m "docs: update streaming protocol spec with ergonomic helpers and review fixes"
```

---

### Task 8: Run full test suite and verify

**Files:** None (validation only)

- [ ] **Step 1: Run full wafer-block tests**

Run: `cd wafer-run && cargo test --lib -p wafer-block`
Expected: all tests pass.

- [ ] **Step 2: Run full workspace check**

Run: `cd wafer-run && cargo check`
Expected: all crates compile.

- [ ] **Step 3: Run full workspace tests**

Run: `cd wafer-run && cargo test`
Expected: all tests pass. If downstream crates have tests that use `Context` or `OutputStream`, they should still work — `call_block_buffered` is a default method and `from_result`/`from_producer` are additive.
