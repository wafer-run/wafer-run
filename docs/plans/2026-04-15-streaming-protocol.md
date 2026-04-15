# Wafer-Run Streaming-Native Block Protocol Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace wafer-run's one-shot `Block::handle(ctx, msg) -> BlockResult` with a streaming-native `async fn handle(ctx, msg, input: InputStream) -> OutputStream` protocol across both monorepos (wafer-run + solobase). Ship one atomic PR.

**Architecture:** New types `InputStream`, `OutputStream`, `OutputSink`, `StreamEvent` replace `BlockResult`/`Action`/`Response`/`Message.data`. Every existing block's handler wraps its current logic in `OutputStream::respond(...)`. HTTP listener gains SSE support. wasmi ABI extended with pull-based host imports for bidirectional streaming. No compat shims — atomic migration.

**Tech Stack:** Rust async-trait, `tokio::sync::mpsc`, `tokio_util::sync::CancellationToken`, `futures::Stream`, axum `Sse`/`Body::from_stream`, `wasmi::TypedResumableCall`, `workers-rs`, `wasm-bindgen-futures`, `web_sys::ReadableStream`.

**Spec:** [2026-04-15-streaming-protocol-design.md](../specs/2026-04-15-streaming-protocol-design.md)

**Work on a dedicated branch.** All tasks land as one PR.

---

## Phase 1: Foundation types in `wafer-block`

Tasks 1–8 add the new types *alongside* existing `BlockResult`/`Action` without touching them yet. Each task leaves the tree compiling.

### Task 1: Add `StreamEvent` enum and its supporting types

**Files:**
- Create: `crates/wafer-block/src/stream.rs`
- Modify: `crates/wafer-block/src/lib.rs` (add `pub mod stream;` and re-exports)
- Test: `crates/wafer-block/src/stream.rs` (inline `#[cfg(test)]` block)

- [ ] **Step 1: Write the failing test**

In `crates/wafer-block/src/stream.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_types::MetaEntry;
    use crate::types::WaferError;

    #[test]
    fn stream_event_variants_construct() {
        let _a = StreamEvent::Chunk(vec![1, 2, 3]);
        let _b = StreamEvent::Meta(MetaEntry::new("Content-Type", "text/event-stream"));
        let _c = StreamEvent::Complete { meta: vec![] };
        let _d = StreamEvent::Error(WaferError::new("test error"));
        let _e = StreamEvent::Drop;
        let _f = StreamEvent::Continue(crate::core_types::Message {
            kind: "forward".into(),
            meta: vec![],
        });
    }

    #[test]
    fn chunk_equality() {
        assert_eq!(
            StreamEvent::Chunk(vec![1, 2, 3]),
            StreamEvent::Chunk(vec![1, 2, 3])
        );
    }

    #[test]
    fn terminal_classification() {
        assert!(!StreamEvent::Chunk(vec![]).is_terminal());
        assert!(!StreamEvent::Meta(MetaEntry::new("k", "v")).is_terminal());
        assert!(StreamEvent::Complete { meta: vec![] }.is_terminal());
        assert!(StreamEvent::Error(WaferError::new("x")).is_terminal());
        assert!(StreamEvent::Drop.is_terminal());
    }
}
```

Also write the first real implementation target at the top of the file:
```rust
use crate::core_types::{MetaEntry, Message};
use crate::types::WaferError;

/// An event in an OutputStream. The stream yields zero-or-more non-terminal
/// events followed by exactly one terminal event.
///
/// Non-terminal: Chunk, Meta
/// Terminal:     Complete, Error, Drop, Continue
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    // ... declared below
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p wafer-block stream::tests`
Expected: compilation error — `StreamEvent` not yet defined.

- [ ] **Step 3: Implement `StreamEvent`**

Replace the stub above with the full type and add it to `crates/wafer-block/src/stream.rs`:
```rust
use crate::core_types::{MetaEntry, Message};
use crate::types::WaferError;

/// An event in an OutputStream. The stream yields zero-or-more non-terminal
/// events followed by exactly one terminal event.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// Body bytes. Non-terminal. Zero or more per stream.
    Chunk(Vec<u8>),

    /// Mid-stream or trailing metadata. Non-terminal. Zero or more per stream.
    Meta(MetaEntry),

    /// Terminal: stream completed normally. Carries trailing metadata.
    Complete { meta: Vec<MetaEntry> },

    /// Terminal: stream failed.
    Error(WaferError),

    /// Terminal: block chose to drop the request (HTTP 204-equivalent).
    /// Valid only with no preceding Chunk or Meta events.
    Drop,

    /// Terminal: forward to another block instead of handling.
    /// Valid only with no preceding Chunk or Meta events.
    Continue(Message),
}

impl StreamEvent {
    /// Whether this event is a terminal (Complete/Error/Drop/Continue).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            StreamEvent::Complete { .. }
                | StreamEvent::Error(_)
                | StreamEvent::Drop
                | StreamEvent::Continue(_)
        )
    }
}
```

Then add to `crates/wafer-block/src/lib.rs`:
```rust
pub mod stream;
pub use stream::StreamEvent;
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p wafer-block stream::tests`
Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block/src/stream.rs crates/wafer-block/src/lib.rs
git commit -m "feat(wafer-block): add StreamEvent enum for streaming protocol"
```

### Task 2: Add `InputStream` type

**Files:**
- Create: `crates/wafer-block/src/streams/input.rs`
- Modify: `crates/wafer-block/src/lib.rs` (register module)
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write the failing test**

In `crates/wafer-block/src/streams/input.rs`:
```rust
use futures::stream::{self, StreamExt};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_stream_yields_no_bytes() {
        let mut s = InputStream::empty();
        assert!(s.next().await.is_none());
    }

    #[tokio::test]
    async fn from_bytes_yields_single_chunk() {
        let mut s = InputStream::from_bytes(b"hello".to_vec());
        let chunk = s.next().await;
        assert_eq!(chunk, Some(b"hello".to_vec()));
        assert!(s.next().await.is_none());
    }

    #[tokio::test]
    async fn from_stream_forwards_chunks() {
        let upstream = stream::iter(vec![vec![1], vec![2, 3], vec![4]]);
        let s = InputStream::from_stream(upstream);
        let chunks: Vec<_> = s.collect().await;
        assert_eq!(chunks, vec![vec![1], vec![2, 3], vec![4]]);
    }

    #[tokio::test]
    async fn collect_to_bytes_concatenates() {
        let s = InputStream::from_stream(stream::iter(vec![
            vec![1, 2],
            vec![3],
            vec![4, 5],
        ]));
        let all = s.collect_to_bytes().await;
        assert_eq!(all, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn cancel_token_is_present() {
        let s = InputStream::empty();
        let _: &CancellationToken = s.cancel_token();
    }
}
```

Stub the type declarations at the top of the file:
```rust
use futures::stream::{BoxStream, Stream, StreamExt};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_util::sync::CancellationToken;

/// One-way streaming input: bytes flowing from a caller into a block's handle().
/// Wraps an inner Stream with a paired CancellationToken.
pub struct InputStream {
    inner: BoxStream<'static, Vec<u8>>,
    cancel: CancellationToken,
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p wafer-block streams::input::tests`
Expected: compile error — constructors not yet defined.

- [ ] **Step 3: Implement `InputStream`**

In `crates/wafer-block/src/streams/input.rs`:
```rust
use futures::stream::{self, BoxStream, Stream, StreamExt};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_util::sync::CancellationToken;

/// One-way streaming input: bytes flowing from a caller into a block's handle().
pub struct InputStream {
    inner: BoxStream<'static, Vec<u8>>,
    cancel: CancellationToken,
}

impl InputStream {
    /// An empty input stream (common default for non-upload calls).
    pub fn empty() -> Self {
        Self {
            inner: Box::pin(stream::empty()),
            cancel: CancellationToken::new(),
        }
    }

    /// A stream containing exactly one chunk.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            inner: Box::pin(stream::once(async move { bytes })),
            cancel: CancellationToken::new(),
        }
    }

    /// Wrap an arbitrary Stream<Item = Vec<u8>>.
    pub fn from_stream<S>(stream: S) -> Self
    where
        S: Stream<Item = Vec<u8>> + Send + 'static,
    {
        Self {
            inner: Box::pin(stream),
            cancel: CancellationToken::new(),
        }
    }

    /// Wrap an arbitrary Stream<Item = Vec<u8>> with a pre-existing cancel token.
    pub fn from_stream_with_cancel<S>(stream: S, cancel: CancellationToken) -> Self
    where
        S: Stream<Item = Vec<u8>> + Send + 'static,
    {
        Self {
            inner: Box::pin(stream),
            cancel,
        }
    }

    /// The cancel token paired with this stream. Fires on drop (via the token owner)
    /// or by explicit .cancel() calls from the consumer.
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Drain the stream into a single Vec by concatenating all chunks.
    /// Convenience for buffered blocks that want the whole body.
    pub async fn collect_to_bytes(mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(chunk) = self.inner.next().await {
            out.extend_from_slice(&chunk);
        }
        out
    }
}

impl Stream for InputStream {
    type Item = Vec<u8>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

#[cfg(test)]
mod tests { /* from Step 1 */ }
```

Register the module in `crates/wafer-block/src/lib.rs`:
```rust
pub mod streams {
    pub mod input;
    pub use input::InputStream;
}
pub use streams::InputStream;
```

Add dependencies if not already present in `crates/wafer-block/Cargo.toml`:
```toml
futures = "0.3"
tokio = { version = "1", features = ["sync", "macros", "rt"] }
tokio-util = "0.7"
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p wafer-block streams::input::tests`
Expected: 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block/src/streams/ crates/wafer-block/src/lib.rs crates/wafer-block/Cargo.toml
git commit -m "feat(wafer-block): add InputStream type with helpers"
```

### Task 3: Add `OutputSink` — the producer handle

**Files:**
- Create: `crates/wafer-block/src/streams/output.rs`
- Modify: `crates/wafer-block/src/lib.rs` / `streams/mod.rs`

- [ ] **Step 1: Write the failing test**

In `crates/wafer-block/src/streams/output.rs` (extending what we'll build):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_types::MetaEntry;
    use crate::stream::StreamEvent;

    #[tokio::test]
    async fn sink_send_chunk_then_complete() {
        let (mut rx, sink, _cancel) = new_streaming_channel(16);

        sink.send_chunk(b"hello".to_vec()).await.unwrap();
        sink.complete(vec![]).await.unwrap();

        let first = rx.recv().await.unwrap();
        assert_eq!(first, StreamEvent::Chunk(b"hello".to_vec()));

        let second = rx.recv().await.unwrap();
        assert!(matches!(second, StreamEvent::Complete { .. }));

        assert!(rx.recv().await.is_none(), "channel should close after terminal");
    }

    #[tokio::test]
    async fn sink_send_chunk_returns_err_when_consumer_dropped() {
        let (rx, sink, _cancel) = new_streaming_channel(16);
        drop(rx);
        let err = sink.send_chunk(b"x".to_vec()).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn sink_complete_consumes_sink() {
        let (_rx, sink, _cancel) = new_streaming_channel(16);
        sink.complete(vec![MetaEntry::new("k", "v")]).await.unwrap();
        // sink is moved; this is a compile-time test.
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p wafer-block streams::output::tests`
Expected: compile errors — no types defined yet.

- [ ] **Step 3: Implement `OutputSink` and the channel constructor**

In `crates/wafer-block/src/streams/output.rs`:
```rust
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::core_types::{MetaEntry, Message};
use crate::stream::StreamEvent;
use crate::types::WaferError;

/// Signals that the consumer dropped the stream.
#[derive(Debug, thiserror::Error)]
#[error("output sink closed: consumer dropped")]
pub struct SinkClosed;

/// Producer handle paired with an OutputStream. The producing task holds this sink
/// and calls send_chunk / send_meta for non-terminal events, then exactly one of
/// complete / error / drop_request / continue_with as the terminal event.
pub struct OutputSink {
    tx: mpsc::Sender<StreamEvent>,
}

impl OutputSink {
    /// Send a body chunk. Awaits when the channel is full (backpressure).
    /// Returns Err if the consumer has dropped the stream.
    pub async fn send_chunk(&self, bytes: Vec<u8>) -> Result<(), SinkClosed> {
        self.tx
            .send(StreamEvent::Chunk(bytes))
            .await
            .map_err(|_| SinkClosed)
    }

    /// Send a mid-stream metadata event (e.g., Content-Type declaration, usage update).
    pub async fn send_meta(&self, entry: MetaEntry) -> Result<(), SinkClosed> {
        self.tx
            .send(StreamEvent::Meta(entry))
            .await
            .map_err(|_| SinkClosed)
    }

    /// Terminal. Must be called exactly once per sink.
    pub async fn complete(self, meta: Vec<MetaEntry>) -> Result<(), SinkClosed> {
        self.tx
            .send(StreamEvent::Complete { meta })
            .await
            .map_err(|_| SinkClosed)
    }

    pub async fn error(self, err: WaferError) -> Result<(), SinkClosed> {
        self.tx
            .send(StreamEvent::Error(err))
            .await
            .map_err(|_| SinkClosed)
    }

    pub async fn drop_request(self) -> Result<(), SinkClosed> {
        self.tx
            .send(StreamEvent::Drop)
            .await
            .map_err(|_| SinkClosed)
    }

    pub async fn continue_with(self, msg: Message) -> Result<(), SinkClosed> {
        self.tx
            .send(StreamEvent::Continue(msg))
            .await
            .map_err(|_| SinkClosed)
    }
}

/// Internal constructor used by OutputStream::new_streaming.
pub(crate) fn new_streaming_channel(
    capacity: usize,
) -> (mpsc::Receiver<StreamEvent>, OutputSink, CancellationToken) {
    let (tx, rx) = mpsc::channel(capacity);
    let cancel = CancellationToken::new();
    (rx, OutputSink { tx }, cancel)
}

#[cfg(test)]
mod tests { /* from Step 1 */ }
```

Register in `crates/wafer-block/src/lib.rs`:
```rust
pub mod streams {
    pub mod input;
    pub mod output;
    pub use input::InputStream;
    pub use output::{OutputSink, SinkClosed};
}
pub use streams::{InputStream, OutputSink, SinkClosed};
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p wafer-block streams::output::tests`
Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block/src/streams/ crates/wafer-block/src/lib.rs
git commit -m "feat(wafer-block): add OutputSink producer handle"
```

### Task 4: Add `OutputStream` — the consumer handle

**Files:**
- Modify: `crates/wafer-block/src/streams/output.rs` (add `OutputStream` after `OutputSink`)

- [ ] **Step 1: Write the failing test**

Append to `crates/wafer-block/src/streams/output.rs` tests:
```rust
#[tokio::test]
async fn respond_is_single_chunk_plus_complete() {
    let stream = OutputStream::respond(b"hello".to_vec());
    let events: Vec<_> = stream.collect().await;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0], StreamEvent::Chunk(b"hello".to_vec()));
    assert!(matches!(events[1], StreamEvent::Complete { .. }));
}

#[tokio::test]
async fn error_is_single_terminal() {
    let stream = OutputStream::error(WaferError::new("boom"));
    let events: Vec<_> = stream.collect().await;
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], StreamEvent::Error(e) if e.message.contains("boom")));
}

#[tokio::test]
async fn new_streaming_yields_pushed_events() {
    let (stream, sink, _cancel) = OutputStream::new_streaming();
    tokio::spawn(async move {
        sink.send_chunk(b"a".to_vec()).await.unwrap();
        sink.send_chunk(b"b".to_vec()).await.unwrap();
        sink.complete(vec![]).await.unwrap();
    });
    let events: Vec<_> = stream.collect().await;
    assert_eq!(events.len(), 3);
    assert_eq!(events[0], StreamEvent::Chunk(b"a".to_vec()));
    assert_eq!(events[1], StreamEvent::Chunk(b"b".to_vec()));
    assert!(matches!(events[2], StreamEvent::Complete { .. }));
}
```

- [ ] **Step 2: Run to see compile errors**

Run: `cargo test -p wafer-block streams::output::tests`

- [ ] **Step 3: Implement `OutputStream`**

Add to `crates/wafer-block/src/streams/output.rs`:
```rust
use futures::stream::{Stream, StreamExt};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_stream::wrappers::ReceiverStream;

/// Consumer handle: a Stream<Item = StreamEvent> that yields chunk/meta events
/// and terminates with exactly one terminal event.
pub struct OutputStream {
    rx: ReceiverStream<StreamEvent>,
    cancel: CancellationToken,
}

impl OutputStream {
    /// Buffered helper: emits one Chunk then Complete with no trailing meta.
    pub fn respond(bytes: Vec<u8>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(2);
        let cancel = CancellationToken::new();
        tokio::spawn(async move {
            let _ = tx.send(StreamEvent::Chunk(bytes)).await;
            let _ = tx
                .send(StreamEvent::Complete { meta: vec![] })
                .await;
        });
        Self {
            rx: ReceiverStream::new(rx),
            cancel,
        }
    }

    /// Buffered error: emits a single terminal Error event.
    pub fn error(err: WaferError) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(1);
        let cancel = CancellationToken::new();
        tokio::spawn(async move {
            let _ = tx.send(StreamEvent::Error(err)).await;
        });
        Self {
            rx: ReceiverStream::new(rx),
            cancel,
        }
    }

    /// Buffered drop: single terminal Drop event.
    pub fn drop_request() -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(1);
        tokio::spawn(async move {
            let _ = tx.send(StreamEvent::Drop).await;
        });
        Self {
            rx: ReceiverStream::new(rx),
            cancel: CancellationToken::new(),
        }
    }

    /// Buffered continue: single terminal Continue event.
    pub fn continue_with(msg: Message) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(1);
        tokio::spawn(async move {
            let _ = tx.send(StreamEvent::Continue(msg)).await;
        });
        Self {
            rx: ReceiverStream::new(rx),
            cancel: CancellationToken::new(),
        }
    }

    /// Streaming constructor. Default capacity 16.
    /// Returns (stream, sink, cancel_token). The sink is used by the producer
    /// task to yield chunks and emit the terminal event.
    pub fn new_streaming() -> (Self, OutputSink, CancellationToken) {
        Self::new_streaming_with_capacity(16)
    }

    pub fn new_streaming_with_capacity(capacity: usize) -> (Self, OutputSink, CancellationToken) {
        let (rx, sink, cancel) = new_streaming_channel(capacity);
        (
            Self {
                rx: ReceiverStream::new(rx),
                cancel: cancel.clone(),
            },
            sink,
            cancel,
        )
    }

    /// The paired cancel token. Fires on drop (via Drop impl below).
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }
}

impl Stream for OutputStream {
    type Item = StreamEvent;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.rx).poll_next(cx)
    }
}

impl Drop for OutputStream {
    fn drop(&mut self) {
        // Dropping the stream cancels the paired token, signaling the producer to abort.
        self.cancel.cancel();
    }
}
```

Also add `tokio-stream` dependency to `crates/wafer-block/Cargo.toml`:
```toml
tokio-stream = "0.1"
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p wafer-block streams::output::tests`
Expected: all output tests pass (6+ tests).

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block/src/streams/output.rs crates/wafer-block/Cargo.toml
git commit -m "feat(wafer-block): add OutputStream consumer handle with drop-triggered cancellation"
```

### Task 5: Add `BufferedResponse` + `collect_buffered` helper

**Files:**
- Modify: `crates/wafer-block/src/streams/output.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/wafer-block/src/streams/output.rs` tests:
```rust
#[tokio::test]
async fn collect_buffered_concatenates_chunks() {
    let (stream, sink, _cancel) = OutputStream::new_streaming();
    tokio::spawn(async move {
        sink.send_chunk(b"he".to_vec()).await.unwrap();
        sink.send_chunk(b"llo".to_vec()).await.unwrap();
        sink.complete(vec![MetaEntry::new("X-Final", "1")]).await.unwrap();
    });
    let buf = stream.collect_buffered().await.unwrap();
    assert_eq!(buf.body, b"hello");
    assert_eq!(buf.meta.len(), 1);
    assert_eq!(buf.meta[0].key, "X-Final");
}

#[tokio::test]
async fn collect_buffered_errors_on_error_terminal() {
    let stream = OutputStream::error(WaferError::new("oops"));
    let result = stream.collect_buffered().await;
    assert!(matches!(result, Err(TerminalNotResponse::Error(_))));
}

#[tokio::test]
async fn collect_buffered_errors_on_drop_terminal() {
    let stream = OutputStream::drop_request();
    let result = stream.collect_buffered().await;
    assert!(matches!(result, Err(TerminalNotResponse::Drop)));
}
```

- [ ] **Step 2: Run to see failures**

Run: `cargo test -p wafer-block streams::output::tests::collect_buffered`
Expected: compile errors.

- [ ] **Step 3: Implement `BufferedResponse`, `TerminalNotResponse`, and `collect_buffered`**

Add to `crates/wafer-block/src/streams/output.rs`:
```rust
/// Convenience buffered view produced by OutputStream::collect_buffered().
#[derive(Debug)]
pub struct BufferedResponse {
    pub body: Vec<u8>,
    pub meta: Vec<MetaEntry>,
}

/// Error returned from collect_buffered when the stream terminated with
/// something other than Complete.
#[derive(Debug)]
pub enum TerminalNotResponse {
    Error(WaferError),
    Drop,
    Continue(Message),
}

impl OutputStream {
    pub async fn collect_buffered(mut self) -> Result<BufferedResponse, TerminalNotResponse> {
        let mut body = Vec::new();
        let mut meta = Vec::new();
        while let Some(evt) = self.next().await {
            match evt {
                StreamEvent::Chunk(bytes) => body.extend_from_slice(&bytes),
                StreamEvent::Meta(entry) => meta.push(entry),
                StreamEvent::Complete { meta: trailing } => {
                    meta.extend(trailing);
                    return Ok(BufferedResponse { body, meta });
                }
                StreamEvent::Error(e) => return Err(TerminalNotResponse::Error(e)),
                StreamEvent::Drop => return Err(TerminalNotResponse::Drop),
                StreamEvent::Continue(msg) => return Err(TerminalNotResponse::Continue(msg)),
            }
        }
        // Stream ended without a terminal event — protocol violation.
        // Treat as error for robustness.
        Err(TerminalNotResponse::Error(WaferError::new(
            "stream ended without terminal event",
        )))
    }

    /// View the body-carrying chunks as a Stream<Item = Vec<u8>>, filtering Meta
    /// events and stopping at the first terminal. Useful for piping one block's
    /// output into another block's InputStream.
    pub fn body_stream(self) -> impl Stream<Item = Vec<u8>> + Send + 'static {
        self.filter_map(|evt| async move {
            match evt {
                StreamEvent::Chunk(bytes) => Some(bytes),
                _ => None,
            }
        })
    }
}
```

Re-export from `crates/wafer-block/src/lib.rs`:
```rust
pub use streams::output::{BufferedResponse, OutputStream, TerminalNotResponse};
```

- [ ] **Step 4: Run**

Run: `cargo test -p wafer-block streams::output::tests`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block/src/streams/output.rs crates/wafer-block/src/lib.rs
git commit -m "feat(wafer-block): add BufferedResponse and collect_buffered helper"
```

### Task 6: Add debug assertion enforcing `Drop`/`Continue` invariant

**Files:**
- Modify: `crates/wafer-block/src/streams/output.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
#[cfg(debug_assertions)]
#[should_panic(expected = "Drop terminal cannot follow")]
async fn sink_drop_request_panics_after_chunk() {
    let (_stream, sink, _cancel) = OutputStream::new_streaming();
    sink.send_chunk(b"x".to_vec()).await.unwrap();
    sink.drop_request().await.unwrap();
}

#[tokio::test]
#[cfg(debug_assertions)]
#[should_panic(expected = "Continue terminal cannot follow")]
async fn sink_continue_panics_after_chunk() {
    let (_stream, sink, _cancel) = OutputStream::new_streaming();
    sink.send_chunk(b"x".to_vec()).await.unwrap();
    sink.continue_with(Message {
        kind: "next".into(),
        meta: vec![],
    })
    .await
    .unwrap();
}
```

- [ ] **Step 2: Run**

Expected: tests don't panic yet; current impl allows it.

- [ ] **Step 3: Add tracking to `OutputSink`**

Modify `OutputSink` in `crates/wafer-block/src/streams/output.rs`:
```rust
pub struct OutputSink {
    tx: mpsc::Sender<StreamEvent>,
    #[cfg(debug_assertions)]
    any_body_sent: std::sync::atomic::AtomicBool,
}

// Update constructor:
pub(crate) fn new_streaming_channel(
    capacity: usize,
) -> (mpsc::Receiver<StreamEvent>, OutputSink, CancellationToken) {
    let (tx, rx) = mpsc::channel(capacity);
    let cancel = CancellationToken::new();
    let sink = OutputSink {
        tx,
        #[cfg(debug_assertions)]
        any_body_sent: std::sync::atomic::AtomicBool::new(false),
    };
    (rx, sink, cancel)
}

impl OutputSink {
    pub async fn send_chunk(&self, bytes: Vec<u8>) -> Result<(), SinkClosed> {
        #[cfg(debug_assertions)]
        self.any_body_sent
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.tx
            .send(StreamEvent::Chunk(bytes))
            .await
            .map_err(|_| SinkClosed)
    }

    pub async fn send_meta(&self, entry: MetaEntry) -> Result<(), SinkClosed> {
        #[cfg(debug_assertions)]
        self.any_body_sent
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.tx
            .send(StreamEvent::Meta(entry))
            .await
            .map_err(|_| SinkClosed)
    }

    pub async fn drop_request(self) -> Result<(), SinkClosed> {
        #[cfg(debug_assertions)]
        assert!(
            !self.any_body_sent.load(std::sync::atomic::Ordering::Relaxed),
            "Drop terminal cannot follow chunks or meta"
        );
        self.tx
            .send(StreamEvent::Drop)
            .await
            .map_err(|_| SinkClosed)
    }

    pub async fn continue_with(self, msg: Message) -> Result<(), SinkClosed> {
        #[cfg(debug_assertions)]
        assert!(
            !self.any_body_sent.load(std::sync::atomic::Ordering::Relaxed),
            "Continue terminal cannot follow chunks or meta"
        );
        self.tx
            .send(StreamEvent::Continue(msg))
            .await
            .map_err(|_| SinkClosed)
    }
    // send_chunk / send_meta / complete / error unchanged in behavior
}
```

- [ ] **Step 4: Run**

Run: `cargo test -p wafer-block streams::output::tests`
Expected: all tests pass, including the new panic tests.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block/src/streams/output.rs
git commit -m "feat(wafer-block): enforce Drop/Continue invariant via debug assertions"
```

### Task 7: Remove `Message.data` field, migrate Message constructors

**Files:**
- Modify: `crates/wafer-block/src/core_types.rs`
- Modify: any test/helper that constructs `Message` within `wafer-block` itself

- [ ] **Step 1: Find all Message constructors**

Run: `rg 'Message \{|Message::new' crates/wafer-block/src/ -n`
Note the results.

- [ ] **Step 2: Write a test for the new shape**

In `crates/wafer-block/src/core_types.rs`:
```rust
#[cfg(test)]
#[test]
fn message_has_no_data_field() {
    let m = Message {
        kind: "POST".to_string(),
        meta: vec![MetaEntry::new("k", "v")],
    };
    assert_eq!(m.kind, "POST");
    assert_eq!(m.meta.len(), 1);
}
```

- [ ] **Step 3: Remove `data` field**

Change in `crates/wafer-block/src/core_types.rs`:
```rust
// Before:
// pub struct Message {
//     pub kind: String,
//     pub data: Vec<u8>,
//     pub meta: Vec<MetaEntry>,
// }

// After:
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub kind: String,
    pub meta: Vec<MetaEntry>,
}
```

Update in-crate constructors within wafer-block to drop the `data:` field.

- [ ] **Step 4: Run**

Run: `cargo check -p wafer-block`
Expected: compiles cleanly (wafer-block internal code no longer depends on `.data`). External crates will break — that's expected and addressed in later tasks.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block/src/core_types.rs
git commit -m "refactor(wafer-block): remove Message.data field (body now via InputStream)"
```

### Task 8: Remove `BlockResult`, `Action`, `Response`, `Result_` from wafer-block

**Files:**
- Modify: `crates/wafer-block/src/core_types.rs`
- Modify: `crates/wafer-block/src/block.rs` (remove reference)
- Modify: `crates/wafer-block/src/lib.rs` (remove re-exports)

- [ ] **Step 1: Locate the types**

Run: `rg '^pub enum Action|^pub struct Response|^pub struct BlockResult|^pub type Result_' crates/wafer-block/src/ -n`

- [ ] **Step 2: Delete the types**

In `crates/wafer-block/src/core_types.rs`, delete:
- `pub enum Action`
- `pub struct Response`
- `pub struct BlockResult`
- `pub type Result_`

In `crates/wafer-block/src/lib.rs`, remove re-exports:
```rust
// Delete any:
// pub use core_types::{Action, BlockResult, Response, Result_};
```

- [ ] **Step 3: Run**

Run: `cargo check -p wafer-block`
Expected: compilation errors in `block.rs` (references `BlockResult` in `Block::handle` signature). That's expected — Task 9 fixes it.

- [ ] **Step 4: Commit with known-broken state**

```bash
git add crates/wafer-block/
git commit -m "refactor(wafer-block): remove BlockResult, Action, Response, Result_

WIP — Block trait still references BlockResult. Fixed in next commit."
```

---

## Phase 2: Block trait + Context signature changes

### Task 9: Update `Block::handle` signature

**Files:**
- Modify: `crates/wafer-block/src/block.rs`

- [ ] **Step 1: Rewrite the Block trait**

Replace `Block::handle` in `crates/wafer-block/src/block.rs`:
```rust
use async_trait::async_trait;
use crate::compat::{MaybeSend, MaybeSync};
use crate::context::Context;
use crate::core_types::Message;
use crate::streams::{InputStream, OutputStream};

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
pub trait Block: MaybeSend + MaybeSync + 'static {
    /// Handle an incoming message. Input bytes (if any) flow through `input`;
    /// events (chunks + terminal) are returned via the OutputStream.
    async fn handle(
        &self,
        ctx: &dyn Context,
        msg: Message,
        input: InputStream,
    ) -> OutputStream;

    // Keep info/capabilities/lifecycle methods unchanged for now; address in later
    // tasks if needed.
    fn info(&self) -> crate::types::BlockInfo;
    fn capabilities(&self) -> crate::types::BlockCapabilities {
        crate::types::BlockCapabilities::default()
    }

    async fn lifecycle(&self, _ctx: &dyn Context, _event: crate::types::LifecycleEvent)
        -> Result<(), crate::types::InitError>
    {
        Ok(())
    }
}
```

- [ ] **Step 2: Update `Context::call_block` signature**

In `crates/wafer-block/src/context.rs`:
```rust
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
pub trait Context: MaybeSend + MaybeSync {
    async fn call_block(
        &self,
        block_name: &str,
        msg: Message,
        input: InputStream,
    ) -> OutputStream;

    // ... other methods unchanged
}
```

- [ ] **Step 3: Run**

Run: `cargo check -p wafer-block`
Expected: compiles clean (no external-crate dependencies yet).

- [ ] **Step 4: Commit**

```bash
git add crates/wafer-block/src/block.rs crates/wafer-block/src/context.rs
git commit -m "refactor(wafer-block): rewrite Block::handle and Context::call_block for streaming"
```

---

## Phase 3: Mass migration of existing blocks

**Template — apply this to every existing block's `Block::handle` impl.**

### Migration template per block

For each block `FooBlock` whose current signature is:
```rust
async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
    // existing logic producing a Vec<u8> body or an Action
}
```

Rewrite to:
```rust
async fn handle(
    &self,
    ctx: &dyn Context,
    msg: Message,
    input: InputStream,
) -> OutputStream {
    let body: Vec<u8> = input.collect_to_bytes().await;
    // existing logic using (msg, body) — returns one of:
    //   - a Vec<u8> response body → OutputStream::respond(body)
    //   - an error → OutputStream::error(WaferError::new("..."))
    //   - a drop → OutputStream::drop_request()
    //   - a continue → OutputStream::continue_with(new_msg)
}
```

Callers inside a block's handle that use `ctx.call_block(...)` change from:
```rust
let mut inner = Message { kind, data, meta };
let res = ctx.call_block("other/block", &mut inner).await;
// use res.response.data
```

To:
```rust
let inner_msg = Message { kind, meta };
let out = ctx
    .call_block("other/block", inner_msg, InputStream::from_bytes(data))
    .await;
let buf = out.collect_buffered().await.map_err(|_| /* error-handling */)?;
// use buf.body
```

---

### Task 10: Migrate service blocks in `wafer-core`

**Targets:**
- `crates/wafer-core/src/service_blocks/database.rs`
- `crates/wafer-core/src/service_blocks/storage.rs`
- `crates/wafer-core/src/interfaces/database/handler.rs` (dispatcher — handles multiple ServiceOps)
- `crates/wafer-core/src/interfaces/storage/handler.rs`

For each `Block::handle` impl, apply the migration template. The handler dispatchers (`database/handler.rs`, `storage/handler.rs`) decode JSON from the input body, call the service, serialize the response — the refactor is: read `body` from `input.collect_to_bytes()` instead of `msg.data`, and wrap returned `Vec<u8>` with `OutputStream::respond(body)`.

- [ ] **Step 1: Update `database.rs` Block impl**

Change signature, delegate to `handler::handle_message(service, ctx, msg, input).await`. Update `handler::handle_message` signature correspondingly to take `(service, ctx, msg: Message, input: InputStream) -> OutputStream`.

- [ ] **Step 2: Update `database/handler.rs`**

Refactor so the dispatcher reads `let body = input.collect_to_bytes().await;` at entry, performs its existing match on operation kind, and returns `OutputStream::respond(serialized_response)` for success cases or `OutputStream::error(err)` for failures.

- [ ] **Step 3: Mirror changes in `storage.rs` and `storage/handler.rs`.**

- [ ] **Step 4: Run**

Run: `cargo check -p wafer-core`
Expected: compiles.

Run: `cargo test -p wafer-core`
Expected: tests pass (internal tests within wafer-core only; cross-crate issues surface later).

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-core/
git commit -m "refactor(wafer-core): migrate service block handlers to streaming protocol"
```

### Task 11: Migrate backend impl blocks (wafer-block-* crates)

**Targets:**
- `crates/wafer-block-sqlite/src/` (the block wrapper around `SqliteDatabaseService`)
- `crates/wafer-block-postgres/src/`
- `crates/wafer-block-local-storage/src/`
- `crates/wafer-block-s3/src/`

Each of these crates has a thin `Block` impl that wraps a service. Apply the template.

- [ ] **Step 1: Update wafer-block-sqlite**

In `crates/wafer-block-sqlite/src/lib.rs` (or wherever the Block impl lives), rewrite the handler.

- [ ] **Step 2: Same for wafer-block-postgres, wafer-block-local-storage, wafer-block-s3.**

- [ ] **Step 3: Run**

```bash
cargo check -p wafer-block-sqlite -p wafer-block-postgres -p wafer-block-local-storage -p wafer-block-s3
```

Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add crates/wafer-block-sqlite crates/wafer-block-postgres crates/wafer-block-local-storage crates/wafer-block-s3
git commit -m "refactor(backends): migrate sqlite/postgres/local-storage/s3 blocks to streaming"
```

### Task 12: Migrate remaining wafer-run block crates

**Targets (apply migration template to each):**
- `wafer-block-auth-validator`
- `wafer-block-config`
- `wafer-block-cors`
- `wafer-block-crypto`
- `wafer-block-iam-guard`
- `wafer-block-inspector`
- `wafer-block-ip-rate-limit`
- `wafer-block-logger`
- `wafer-block-monitoring`
- `wafer-block-network`
- `wafer-block-readonly-guard`
- `wafer-block-router`
- `wafer-block-security-headers`
- `wafer-block-web`

- [ ] **Step 1: For each crate, locate and update the `Block::handle` impl.**

Use `rg 'impl Block for' crates/` to enumerate; apply the migration template.

- [ ] **Step 2: Run after each set of 3–4 crates**

```bash
cargo check --workspace
```

- [ ] **Step 3: Commit per-crate or in logical groups**

```bash
git add crates/wafer-block-auth-validator crates/wafer-block-config crates/wafer-block-cors
git commit -m "refactor(blocks): migrate auth-validator, config, cors to streaming handle"
```

Repeat for remaining crates with similar grouped commits.

### Task 13: Migrate solobase-core blocks

**Targets in `solobase/crates/solobase-core/src/blocks/`:**
- `admin/` (multiple files)
- `auth/`
- `email.rs`
- `files/`
- `legalpages/`
- `llm/` (existing — will be rewritten in Spec 2; for this plan, just make the current handler compile with the new signature)
- `messages/`
- `network.rs`
- `products/`
- `projects/`
- `provider_llm/` (same note as `llm/`)
- `local_llm.rs` (same note)
- `rate_limit.rs`
- `router.rs`
- `storage.rs`
- `system.rs`
- `userportal.rs`

- [ ] **Step 1: For each block, apply the migration template.**

- [ ] **Step 2: Run workspace-wide check after every ~5 block migrations**

```bash
cd /home/joris/Programs/suppers-ai/workspace/solobase
cargo check -p solobase-core
```

- [ ] **Step 3: Commit in logical groups**

```bash
git add crates/solobase-core/src/blocks/admin crates/solobase-core/src/blocks/auth
git commit -m "refactor(solobase-core): migrate admin, auth blocks to streaming handle"
```

Continue until all solobase-core blocks migrated.

---

## Phase 4: Native dispatcher

### Task 14: Update `RuntimeContext::call_block` in wafer-run

**Files:**
- Modify: `crates/wafer-run/src/context.rs`

- [ ] **Step 1: Write an integration test**

Create `crates/wafer-run/tests/streaming_dispatch.rs`:
```rust
use wafer_block::block::Block;
use wafer_block::core_types::Message;
use wafer_block::streams::{InputStream, OutputStream};
use wafer_run::Wafer;
use async_trait::async_trait;

struct EchoBlock;

#[async_trait]
impl Block for EchoBlock {
    async fn handle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        _msg: Message,
        input: InputStream,
    ) -> OutputStream {
        let body = input.collect_to_bytes().await;
        OutputStream::respond(body)
    }

    fn info(&self) -> wafer_block::types::BlockInfo {
        wafer_block::types::BlockInfo::new("test/echo")
    }
}

#[tokio::test]
async fn native_dispatch_passes_streams_through() {
    let mut wafer = Wafer::new();
    wafer.register_block("test/echo".into(), std::sync::Arc::new(EchoBlock))
        .expect("register");
    let ctx = wafer.context();
    let msg = Message { kind: "test".into(), meta: vec![] };
    let input = InputStream::from_bytes(b"hello".to_vec());
    let output = ctx.call_block("test/echo", msg, input).await;
    let buf = output.collect_buffered().await.unwrap();
    assert_eq!(buf.body, b"hello");
}
```

- [ ] **Step 2: Run to see the failure**

```bash
cd /home/joris/Programs/suppers-ai/workspace/wafer-run
cargo test -p wafer-run --test streaming_dispatch
```

Expected: compile errors in `RuntimeContext`.

- [ ] **Step 3: Rewrite `RuntimeContext::call_block`**

In `crates/wafer-run/src/context.rs`, replace the old call_block impl:
```rust
#[async_trait]
impl Context for RuntimeContext {
    async fn call_block(
        &self,
        block_name: &str,
        msg: Message,
        input: InputStream,
    ) -> OutputStream {
        let block = match self.all_blocks.get(block_name) {
            Some(b) => b.clone(),
            None => {
                return OutputStream::error(WaferError::new(format!(
                    "block not found: {}",
                    block_name
                )))
            }
        };
        // Construct sub-context with updated node_id / caller_id.
        let sub_ctx = self.sub_context_for(block_name);
        block.handle(&sub_ctx, msg, input).await
    }
    // other methods unchanged
}
```

- [ ] **Step 4: Run the test**

Expected: passes.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-run/src/context.rs crates/wafer-run/tests/streaming_dispatch.rs
git commit -m "feat(wafer-run): native dispatcher passes streams through to handle"
```

---

## Phase 5: HTTP adapter

### Task 15: Add `wafer_output_to_response` — the buffered path

**Files:**
- Modify: `crates/wafer-block-http-listener/src/lib.rs`

- [ ] **Step 1: Write test for buffered response**

```rust
#[tokio::test]
async fn buffered_output_becomes_200_with_body() {
    let out = OutputStream::respond(b"hello".to_vec());
    let resp = wafer_output_to_response(out).await;
    assert_eq!(resp.status(), 200);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body, b"hello".as_ref());
}

#[tokio::test]
async fn drop_output_becomes_204() {
    let out = OutputStream::drop_request();
    let resp = wafer_output_to_response(out).await;
    assert_eq!(resp.status(), 204);
}

#[tokio::test]
async fn error_output_becomes_500_with_json() {
    let out = OutputStream::error(WaferError::new("boom"));
    let resp = wafer_output_to_response(out).await;
    assert_eq!(resp.status(), 500);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("error").is_some());
}
```

- [ ] **Step 2: Run**

Expected: compile error.

- [ ] **Step 3: Implement buffered path**

Delete old `wafer_result_to_response`. Add new function:
```rust
pub async fn wafer_output_to_response(
    mut stream: OutputStream,
) -> axum::http::Response<axum::body::Body> {
    // Peek the first event to decide buffered vs streaming.
    let mut buffered_body = Vec::new();
    let mut meta: Vec<MetaEntry> = Vec::new();
    let mut streaming_sse = false;

    while let Some(evt) = stream.next().await {
        match evt {
            StreamEvent::Meta(entry) => {
                if entry.key.eq_ignore_ascii_case("Content-Type")
                    && entry.value == "text/event-stream"
                {
                    streaming_sse = true;
                    // Switch to streaming response with the remaining stream.
                    return stream_as_sse(meta, stream).await;
                }
                meta.push(entry);
            }
            StreamEvent::Chunk(bytes) => {
                if streaming_sse {
                    // Shouldn't reach here since we return earlier.
                    unreachable!();
                }
                buffered_body.extend_from_slice(&bytes);
            }
            StreamEvent::Complete { meta: trailing } => {
                meta.extend(trailing);
                return buffered_response(200, buffered_body, meta);
            }
            StreamEvent::Drop => {
                return buffered_response(204, Vec::new(), meta);
            }
            StreamEvent::Continue(_msg) => {
                // TODO: re-dispatch; simplified for now as 500.
                return buffered_response(
                    500,
                    b"continue not yet supported by listener".to_vec(),
                    meta,
                );
            }
            StreamEvent::Error(err) => {
                let body = serde_json::json!({ "error": err.message }).to_string();
                return buffered_response(500, body.into_bytes(), meta);
            }
        }
    }
    buffered_response(
        500,
        b"stream ended without terminal event".to_vec(),
        Vec::new(),
    )
}

fn buffered_response(
    status: u16,
    body: Vec<u8>,
    meta: Vec<MetaEntry>,
) -> axum::http::Response<axum::body::Body> {
    let mut builder = axum::http::Response::builder().status(status);
    for entry in meta {
        builder = builder.header(&entry.key, &entry.value);
    }
    builder
        .body(axum::body::Body::from(body))
        .unwrap()
}

// stream_as_sse — implemented in Task 16.
async fn stream_as_sse(
    _trailing_meta: Vec<MetaEntry>,
    _stream: OutputStream,
) -> axum::http::Response<axum::body::Body> {
    unimplemented!("streaming SSE added in next task")
}
```

- [ ] **Step 4: Run buffered tests**

Run: `cargo test -p wafer-block-http-listener wafer_output_to_response`
Expected: buffered-path tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block-http-listener/
git commit -m "feat(http-listener): buffered wafer_output_to_response replaces wafer_result_to_response"
```

### Task 16: Add SSE streaming path

**Files:**
- Modify: `crates/wafer-block-http-listener/src/lib.rs`

- [ ] **Step 1: Write test for SSE**

```rust
#[tokio::test]
async fn sse_declaration_switches_to_streaming_body() {
    let (stream, sink, _cancel) = OutputStream::new_streaming();
    tokio::spawn(async move {
        sink.send_meta(MetaEntry::new("Content-Type", "text/event-stream"))
            .await
            .unwrap();
        sink.send_chunk(b"hello".to_vec()).await.unwrap();
        sink.send_chunk(b"world".to_vec()).await.unwrap();
        sink.complete(vec![]).await.unwrap();
    });
    let resp = wafer_output_to_response(stream).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let s = std::str::from_utf8(&body).unwrap();
    assert!(s.contains("data: hello"));
    assert!(s.contains("data: world"));
}
```

- [ ] **Step 2: Run**

Expected: `unimplemented!` panic.

- [ ] **Step 3: Implement `stream_as_sse`**

```rust
use futures::StreamExt;

async fn stream_as_sse(
    trailing_meta: Vec<MetaEntry>,
    stream: OutputStream,
) -> axum::http::Response<axum::body::Body> {
    let sse_events = stream.filter_map(|evt| async move {
        match evt {
            StreamEvent::Chunk(bytes) => {
                // One SSE frame per chunk. Payload is the UTF-8 rendering of the bytes.
                let payload = String::from_utf8_lossy(&bytes).to_string();
                Some(Ok::<_, std::convert::Infallible>(
                    format!("data: {}\n\n", payload).into_bytes(),
                ))
            }
            StreamEvent::Meta(_) => None, // no in-band meta mid-stream on the wire
            StreamEvent::Complete { .. } => None, // body just ends
            StreamEvent::Drop => None,
            StreamEvent::Continue(_) => None,
            StreamEvent::Error(err) => {
                let frame = format!(
                    "event: error\ndata: {}\n\n",
                    serde_json::json!({ "error": err.message })
                );
                Some(Ok(frame.into_bytes()))
            }
        }
    });

    let body = axum::body::Body::from_stream(sse_events);
    let mut builder = axum::http::Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive");
    for entry in trailing_meta {
        builder = builder.header(&entry.key, &entry.value);
    }
    builder.body(body).unwrap()
}
```

- [ ] **Step 4: Run**

Expected: SSE test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block-http-listener/
git commit -m "feat(http-listener): SSE streaming path for streaming-content-type responses"
```

### Task 17: InputStream from axum request body + cancellation

**Files:**
- Modify: `crates/wafer-block-http-listener/src/lib.rs`

- [ ] **Step 1: Write test**

```rust
#[tokio::test]
async fn request_body_becomes_input_stream() {
    let body = axum::body::Body::from("request body bytes");
    let input = input_stream_from_axum_body(body);
    let collected = input.collect_to_bytes().await;
    assert_eq!(collected, b"request body bytes");
}
```

- [ ] **Step 2 + 3: Implement**

```rust
pub fn input_stream_from_axum_body(body: axum::body::Body) -> InputStream {
    use futures::StreamExt;
    let cancel = CancellationToken::new();
    let byte_stream = body.into_data_stream().filter_map(|chunk_result| async move {
        match chunk_result {
            Ok(bytes) => Some(bytes.to_vec()),
            Err(_) => None,
        }
    });
    InputStream::from_stream_with_cancel(byte_stream, cancel)
}
```

Wire the request-body → InputStream in the listener's request handler (the function that dispatches routes to `Block::handle`): replace the old `msg.data = body_bytes` with an explicit `input = input_stream_from_axum_body(req.into_body())`.

- [ ] **Step 4: Run**

`cargo test -p wafer-block-http-listener input_stream_from_axum_body`

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block-http-listener/
git commit -m "feat(http-listener): build InputStream from axum request body"
```

### Task 18: Wire client-disconnect to cancellation

**Files:**
- Modify: `crates/wafer-block-http-listener/src/lib.rs`

- [ ] **Step 1: Write test**

```rust
#[tokio::test]
async fn client_disconnect_cancels_stream() {
    // Launch a server that streams slowly; drop the response's body stream;
    // assert the producer's cancel token fires.
    // (Full impl uses axum test harness; scaffold test in this task.)
}
```

- [ ] **Step 2+3: Implement**

In the request-handler function: after building the `OutputStream`, fork a task that watches the axum response sender's dropped-ness and calls `output.cancel_token().cancel()`. For axum 0.7+, the `Body::from_stream` + `Drop` of the hyper connection already signals via the stream; ensure the `OutputStream`'s token fires in that case via the existing Drop impl on `OutputStream`.

- [ ] **Step 4: Run**

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block-http-listener/
git commit -m "feat(http-listener): propagate client disconnect to CancellationToken"
```

---

## Phase 6: Browser service worker adapter (`solobase-web`)

### Task 19: Replace Request.data with InputStream from fetch

**Files:**
- Modify: `solobase/crates/solobase-web/src/` (service worker request handler)

- [ ] **Step 1: Write browser-test**

Use `wasm-bindgen-test`. Test that a `fetch(url, { body: 'hello' })` arriving at the SW handler produces an `InputStream` that collects to `b"hello"`.

- [ ] **Step 2: Implement**

Add `web_sys::ReadableStream → InputStream` adapter in `solobase-web/src/http_adapter.rs`:
```rust
pub fn input_stream_from_web_request(
    req: &web_sys::Request,
) -> InputStream {
    let body = req.body();
    // Convert ReadableStream to Rust Stream via wasm_streams::ReadableStream.
    let rs = wasm_streams::ReadableStream::from_raw(body.unchecked_into());
    let byte_stream = rs.into_stream().filter_map(|chunk_result| async move {
        match chunk_result {
            Ok(js_val) => {
                let uint8 = js_sys::Uint8Array::new(&js_val);
                Some(uint8.to_vec())
            }
            Err(_) => None,
        }
    });
    InputStream::from_stream(byte_stream)
}
```

Add dependency `wasm-streams = "0.4"` to `solobase-web/Cargo.toml`.

- [ ] **Step 3: Run**

```bash
cd /home/joris/Programs/suppers-ai/workspace/solobase
wasm-pack test --chrome --headless crates/solobase-web
```

- [ ] **Step 4: Commit**

```bash
git add crates/solobase-web/
git commit -m "feat(solobase-web): InputStream from web_sys Request body"
```

### Task 20: OutputStream → ReadableStream Response body

**Files:**
- Modify: `solobase/crates/solobase-web/src/http_adapter.rs`

- [ ] **Step 1: Write test**

Serve an `OutputStream::respond(b"hello".to_vec())` and `OutputStream::new_streaming()` cases; assert browser `fetch()` receives expected bodies.

- [ ] **Step 2: Implement buffered + SSE paths**

For buffered output: collect, build a `Response` with the concatenated body.

For streaming output (detected by `Content-Type: text/event-stream` first meta):
```rust
pub async fn wafer_output_to_web_response(
    stream: OutputStream,
) -> web_sys::Response {
    // Similar peek-first logic as axum version.
    // For streaming path:
    let rs = wasm_streams::ReadableStream::from_async_iter(sse_events_iter(stream));
    let init = web_sys::ResponseInit::new();
    init.set_status(200);
    let headers = web_sys::Headers::new().unwrap();
    headers.set("Content-Type", "text/event-stream").unwrap();
    init.set_headers(&headers);
    web_sys::Response::new_with_opt_readable_stream_and_init(
        Some(&rs.into_raw()),
        &init,
    )
    .unwrap()
}
```

- [ ] **Step 3: Run**

- [ ] **Step 4: Commit**

```bash
git add crates/solobase-web/
git commit -m "feat(solobase-web): OutputStream → ReadableStream with SSE support"
```

---

## Phase 7: Cloudflare Workers adapter (`solobase-cloudflare`)

### Task 21: CF Workers InputStream + Response adapter

**Files:**
- Modify: `solobase/crates/solobase-cloudflare/src/`

- [ ] **Step 1: Implement `InputStream` from `worker::Request::stream()`**

```rust
pub fn input_stream_from_cf_request(req: &worker::Request) -> InputStream {
    let byte_stream = req.stream().unwrap().filter_map(|chunk_result| async move {
        chunk_result.ok().map(|chunk| chunk.to_vec())
    });
    InputStream::from_stream(byte_stream)
}
```

- [ ] **Step 2: Implement response adapter**

```rust
pub async fn wafer_output_to_cf_response(
    stream: OutputStream,
) -> worker::Response {
    // Buffered vs streaming detection as in axum case.
    // Streaming: worker::Response::from_stream(sse_events)
}
```

- [ ] **Step 3: Run**

Build locally via `wrangler dev`; run integration test.

- [ ] **Step 4: Commit**

```bash
git add crates/solobase-cloudflare/
git commit -m "feat(solobase-cloudflare): InputStream + OutputStream → worker::Response"
```

---

## Phase 8: wasmi host ↔ guest ABI

### Task 22: Design + add host imports to wasmi loader

**Files:**
- Modify: `crates/wafer-run/src/wasm/wasmi_loader.rs`

- [ ] **Step 1: Identify existing import registration site**

Run: `rg '__wafer_host_call_block' crates/wafer-run/src/wasm/ -n`

- [ ] **Step 2: Add new imports (skeletons)**

Define host function signatures — one host import per new ABI function:
- `__wafer_host_call_begin(name_ptr, name_len, msg_ptr, msg_len) -> call_handle: u32`
- `__wafer_host_call_input_send(call_handle, chunk_ptr, chunk_len) -> result: u32`
- `__wafer_host_call_input_close(call_handle)`
- `__wafer_host_call_output_recv(call_handle, buf_ptr, buf_cap) -> (event_kind: u32, len_written: u32)` — resumable trap
- `__wafer_host_call_cancel(call_handle)`
- `__wafer_host_call_end(call_handle)`

Implement host-side state: a `CallRegistry` keyed by `call_handle: u32` mapping to a running `(OutputStream, cancel_token, input_sink)`.

- [ ] **Step 3: Run**

Compile-only check; behavioral tests in next task.

- [ ] **Step 4: Commit**

```bash
git add crates/wafer-run/src/wasm/
git commit -m "feat(wasm): add streaming ABI host imports (skeletons)"
```

### Task 23: Integrate trap-resume for output_recv

**Files:**
- Modify: `crates/wafer-run/src/wasm/wasmi_loader.rs`

- [ ] **Step 1: Write integration test**

Write a minimal Rust guest block that uses the new ABI to receive a stream from the host; run via wasmi and assert received events match.

- [ ] **Step 2: Implement trap-resume loop**

Extend the existing `TypedResumableCall` loop to handle `output_recv` pending state: when an event isn't queued yet, record `call_handle` as pending output-recv and resume when next event arrives on the underlying `OutputStream.rx`.

- [ ] **Step 3: Run**

- [ ] **Step 4: Commit**

```bash
git add crates/wafer-run/src/wasm/
git commit -m "feat(wasm): trap-resume loop for output_recv"
```

### Task 24: Guest SDK streaming wrapper

**Files:**
- Modify: `sdks/rust/src/` (guest SDK)

- [ ] **Step 1: Write test in the guest SDK crate**

- [ ] **Step 2: Add `InputStream` / `OutputStream` wrappers that use host imports**

Expose the same API surface (`InputStream::empty()`, `OutputStream::respond()`, `send_chunk`, etc.) in the guest SDK, routing to the wasmi host imports under the hood. Blocks in guest WASM modules look identical to native blocks.

- [ ] **Step 3: Run**

- [ ] **Step 4: Commit**

```bash
git add sdks/rust/
git commit -m "feat(sdk): guest-side streaming protocol via host imports"
```

### Task 25: Symmetric guest exports (host-calling-into-guest)

**Files:**
- Modify: `crates/wafer-run/src/wasm/wasmi_loader.rs`
- Modify: `sdks/rust/src/`

- [ ] **Step 1: Add guest export ABI**

- `__wafer_guest_handle_begin(msg_ptr, msg_len) -> call_handle: u32`
- `__wafer_guest_handle_input_recv(call_handle, buf_ptr, buf_cap) -> (len_read: u32, end: u32)`
- `__wafer_guest_handle_output_send(call_handle, event_kind: u32, payload_ptr, payload_len) -> result: u32`
- `__wafer_guest_handle_cancel(call_handle)`
- `__wafer_guest_handle_end(call_handle)`

The host, when invoking a guest block's `handle`, uses these exports to feed input chunks and drain output chunks.

- [ ] **Step 2: Implement in guest SDK via `#[wafer_block]` proc macro**

The macro already generates WASM ABI exports; extend it to generate the new streaming exports.

- [ ] **Step 3: Run**

Integration test: register a guest block, call it with a streaming input, assert output matches.

- [ ] **Step 4: Commit**

```bash
git add crates/wafer-run/src/wasm/ sdks/rust/ crates/wafer-block-macro/
git commit -m "feat(wasm): symmetric guest export ABI for Block::handle"
```

---

## Phase 9: Cleanup

### Task 26: Delete `ctx.call_block` old-shape references

**Files:**
- Any remaining files still using the old three-field `Result_`/`BlockResult` shape

- [ ] **Step 1: Workspace-wide grep**

```bash
rg 'BlockResult|Action::Respond|Action::Drop|\.response\.data|Result_' --type rust
```

- [ ] **Step 2: Fix every remaining occurrence** using the migration template.

- [ ] **Step 3: Run workspace-wide checks**

```bash
cd /home/joris/Programs/suppers-ai/workspace/wafer-run && cargo check --workspace
cd /home/joris/Programs/suppers-ai/workspace/solobase && cargo check --workspace
```

Expected: both workspaces compile clean.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "chore: remove remaining references to old BlockResult/Action shapes"
```

### Task 27: Remove `discovery::streaming = false` hardcoded field

**Files:**
- Modify: `crates/wafer-core/src/discovery.rs`

- [ ] **Step 1: Search for the field**

```bash
rg '"streaming"\s*:\s*false' crates/
```

- [ ] **Step 2: Remove** the hardcoded field (all blocks stream now; either remove the field entirely or make it `true` universally and deprecate).

- [ ] **Step 3: Run**

- [ ] **Step 4: Commit**

```bash
git add crates/wafer-core/src/discovery.rs
git commit -m "chore(discovery): remove hardcoded streaming=false — streaming is universal"
```

---

## Phase 10: Integration testing

### Task 28: End-to-end native streaming test

**Files:**
- Create: `crates/wafer-run/tests/streaming_e2e.rs`

- [ ] **Step 1: Write tests**

```rust
#[tokio::test]
async fn pipeline_streams_end_to_end() {
    // Two blocks: uppercase + reverse. Pipe output of first into input of second.
    // Assert full output is correct and never buffered fully at any stage.
}

#[tokio::test]
async fn cancellation_propagates_end_to_end() {
    // Slow producer block. Consumer drops output stream. Producer's cancel token fires
    // within 100ms.
}

#[tokio::test]
async fn backpressure_slows_producer() {
    // Slow consumer. Producer tries to send at full throttle. Measure producer wait
    // time > 0 (vs immediate send if unbounded).
}
```

- [ ] **Step 2–5: Run, implement test helpers, commit.**

### Task 29: HTTP SSE end-to-end test

**Files:**
- Create: `crates/wafer-block-http-listener/tests/sse_e2e.rs`

- [ ] **Step 1: Write test**

Use axum test harness; register a streaming block; issue `GET /stream`; assert SSE frames arrive in order.

- [ ] **Step 2–5: Implement + commit.**

### Task 30: wasmi guest streaming round-trip test

**Files:**
- Create: `crates/wafer-run/tests/wasm_streaming.rs`
- Create: `examples/wasm-streaming-guest/` (minimal guest block)

- [ ] **Step 1: Build a wasmi guest block that streams N chunks + Complete.**
- [ ] **Step 2: Host test loads the guest, issues a call, receives the stream, asserts events match.**
- [ ] **Step 3–5: Implement + commit.**

---

## Phase 11: Final verification

### Task 31: Full workspace test run

- [ ] **Step 1: Run test suites in both workspaces**

```bash
cd /home/joris/Programs/suppers-ai/workspace/wafer-run && cargo test --workspace
cd /home/joris/Programs/suppers-ai/workspace/solobase && cargo test --workspace
```

Expected: all tests pass.

- [ ] **Step 2: Run browser tests for solobase-web**

```bash
cd /home/joris/Programs/suppers-ai/workspace/solobase
wasm-pack test --chrome --headless crates/solobase-web
```

- [ ] **Step 3: Run CF workers integration (wrangler dev)**

Manually exercise the endpoints.

- [ ] **Step 4: Check for dead code / remaining references**

```bash
rg 'BlockResult|Action::Respond|\.data\s*=' --type rust
```

Should return zero relevant results.

- [ ] **Step 5: Commit**

Nothing to commit at this step — just verification. If changes needed, commit as `fix(...)`.

### Task 32: Update docs

**Files:**
- Modify: `README.md` or block-author docs
- Modify: `docs/` (if any) for block-author migration notes

- [ ] **Step 1: Document the new `Block::handle` signature**

- [ ] **Step 2: Document the migration checklist for external block authors** (who maintain blocks outside these monorepos)

- [ ] **Step 3: Commit**

```bash
git add README.md docs/
git commit -m "docs: document streaming-native block protocol"
```

---

## Migration reference (quick sheet)

For readers migrating individual blocks, here's the one-page reference:

### Old handle signature
```rust
async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
    let body = &msg.data;
    // logic returning Vec<u8>
    Ok(BlockResult {
        action: Action::Respond,
        response: Some(Response { data: body_out, meta }),
        ..Default::default()
    })
}
```

### New handle signature
```rust
async fn handle(
    &self,
    ctx: &dyn Context,
    msg: Message,
    input: InputStream,
) -> OutputStream {
    let body = input.collect_to_bytes().await;
    // same logic
    OutputStream::respond(body_out)
}
```

### Old ctx.call_block
```rust
let mut inner = Message { kind, data, meta };
let res = ctx.call_block("other", &mut inner).await?;
let bytes = res.response.map(|r| r.data).unwrap_or_default();
```

### New ctx.call_block
```rust
let msg = Message { kind, meta };
let out = ctx.call_block("other", msg, InputStream::from_bytes(data)).await;
let buf = out.collect_buffered().await?;
let bytes = buf.body;
```

### Constructors cheat sheet
- Buffered response: `OutputStream::respond(bytes)`
- Error: `OutputStream::error(WaferError::new("..."))`
- 204: `OutputStream::drop_request()`
- Forward: `OutputStream::continue_with(msg)`
- Streaming: `let (stream, sink, cancel) = OutputStream::new_streaming(); tokio::spawn(async move { sink.send_chunk(...).await?; sink.complete(vec![]).await?; }); stream`
