use futures::stream::Stream;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::core_types::{Message, MetaEntry, WaferError};
use crate::stream::StreamEvent;

/// Signals that the consumer dropped the stream.
#[derive(Debug, thiserror::Error)]
#[error("output sink closed: consumer dropped")]
pub struct SinkClosed;

/// Producer handle paired with an OutputStream. The producing task holds this sink
/// and calls send_chunk / send_meta for non-terminal events, then exactly one of
/// complete / error / drop_request / continue_with as the terminal event.
pub struct OutputSink {
    tx: mpsc::Sender<StreamEvent>,
    terminal_sent: bool,
    #[cfg(debug_assertions)]
    any_body_sent: std::sync::atomic::AtomicBool,
}

impl OutputSink {
    /// Send a body chunk. Awaits when the channel is full (backpressure).
    /// Returns Err if the consumer has dropped the stream.
    pub async fn send_chunk(&self, bytes: Vec<u8>) -> Result<(), SinkClosed> {
        #[cfg(debug_assertions)]
        self.any_body_sent
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.tx
            .send(StreamEvent::Chunk(bytes))
            .await
            .map_err(|_| SinkClosed)
    }

    /// Send a mid-stream metadata event (e.g., Content-Type declaration).
    pub async fn send_meta(&self, entry: MetaEntry) -> Result<(), SinkClosed> {
        #[cfg(debug_assertions)]
        self.any_body_sent
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.tx
            .send(StreamEvent::Meta(entry))
            .await
            .map_err(|_| SinkClosed)
    }

    /// Terminal. Must be called exactly once per sink.
    pub async fn complete(mut self, meta: Vec<MetaEntry>) -> Result<(), SinkClosed> {
        self.terminal_sent = true;
        self.tx
            .send(StreamEvent::Complete { meta })
            .await
            .map_err(|_| SinkClosed)
    }

    /// Terminal. The block encountered an error.
    pub async fn error(mut self, err: WaferError) -> Result<(), SinkClosed> {
        self.terminal_sent = true;
        self.tx
            .send(StreamEvent::Error(Box::new(err)))
            .await
            .map_err(|_| SinkClosed)
    }

    /// Terminal. The block chose to drop the request (HTTP 204-equivalent).
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

    /// Terminal. Forward to another block instead of handling.
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
}

impl Drop for OutputSink {
    fn drop(&mut self) {
        if !self.terminal_sent {
            let _ = self.tx.try_send(StreamEvent::Complete { meta: vec![] });
        }
    }
}

/// Internal constructor used by OutputStream::new_streaming.
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

/// Consumer handle: a `Stream<Item = StreamEvent>` that yields chunk / meta events
/// and terminates with exactly one terminal event.
///
/// When dropped, fires the paired `CancellationToken` to signal the producer to abort.
pub struct OutputStream {
    rx: ReceiverStream<StreamEvent>,
    cancel: CancellationToken,
}

impl OutputStream {
    /// Buffered helper: emits one Chunk then Complete with no trailing meta.
    ///
    /// Pre-fills the mpsc channel synchronously via `try_send`; no spawn needed.
    pub fn respond(bytes: Vec<u8>) -> Self {
        let (tx, rx) = mpsc::channel::<StreamEvent>(2);
        let cancel = CancellationToken::new();
        let _ = tx.try_send(StreamEvent::Chunk(bytes));
        let _ = tx.try_send(StreamEvent::Complete { meta: vec![] });
        Self {
            rx: ReceiverStream::new(rx),
            cancel,
        }
    }

    /// Buffered helper: emits one Chunk (if non-empty) then Complete with the
    /// given trailing meta.
    pub fn respond_with_meta(bytes: Vec<u8>, meta: Vec<crate::core_types::MetaEntry>) -> Self {
        let (tx, rx) = mpsc::channel::<StreamEvent>(2);
        let cancel = CancellationToken::new();
        if !bytes.is_empty() {
            let _ = tx.try_send(StreamEvent::Chunk(bytes));
        }
        let _ = tx.try_send(StreamEvent::Complete { meta });
        Self {
            rx: ReceiverStream::new(rx),
            cancel,
        }
    }

    /// Buffered helper: emits a single Error terminal event.
    pub fn error(err: WaferError) -> Self {
        let (tx, rx) = mpsc::channel::<StreamEvent>(1);
        let cancel = CancellationToken::new();
        let _ = tx.try_send(StreamEvent::Error(Box::new(err)));
        Self {
            rx: ReceiverStream::new(rx),
            cancel,
        }
    }

    /// Buffered helper: emits a single Drop terminal event.
    pub fn drop_request() -> Self {
        let (tx, rx) = mpsc::channel::<StreamEvent>(1);
        let cancel = CancellationToken::new();
        let _ = tx.try_send(StreamEvent::Drop);
        Self {
            rx: ReceiverStream::new(rx),
            cancel,
        }
    }

    /// Buffered helper: emits a single Continue terminal event.
    pub fn continue_with(msg: Message) -> Self {
        let (tx, rx) = mpsc::channel::<StreamEvent>(1);
        let cancel = CancellationToken::new();
        let _ = tx.try_send(StreamEvent::Continue(msg));
        Self {
            rx: ReceiverStream::new(rx),
            cancel,
        }
    }

    /// Create a streaming triple `(OutputStream, OutputSink, CancellationToken)` with
    /// the default channel capacity of 16.
    pub fn new_streaming() -> (Self, OutputSink, CancellationToken) {
        Self::new_streaming_with_capacity(16)
    }

    /// Create a streaming triple with the given channel capacity.
    pub fn new_streaming_with_capacity(capacity: usize) -> (Self, OutputSink, CancellationToken) {
        let (rx, sink, cancel) = new_streaming_channel(capacity);
        let stream = Self {
            rx: ReceiverStream::new(rx),
            cancel: cancel.clone(),
        };
        (stream, sink, cancel)
    }

    /// Returns a reference to the paired `CancellationToken`.
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Convert a `Result<Vec<u8>, WaferError>` into an `OutputStream`.
    ///
    /// `Ok(bytes)` → `respond(bytes)`, `Err(e)` → `error(e)`.
    pub fn from_result(result: Result<Vec<u8>, WaferError>) -> Self {
        match result {
            Ok(bytes) => Self::respond(bytes),
            Err(e) => Self::error(e),
        }
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
        self.cancel.cancel();
    }
}

/// The collected view of a complete output stream, produced by `OutputStream::collect_buffered`.
#[derive(Debug)]
pub struct BufferedResponse {
    pub body: Vec<u8>,
    pub meta: Vec<MetaEntry>,
}

/// Error returned by `OutputStream::collect_buffered` when the stream terminates
/// with something other than `Complete`.
#[derive(Debug)]
pub enum TerminalNotResponse {
    Error(WaferError),
    Drop,
    Continue(Message),
    /// Stream ended without emitting any terminal event (protocol violation).
    Malformed,
}

impl std::fmt::Display for TerminalNotResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Error(e) => write!(f, "block error: {e}"),
            Self::Drop => write!(f, "block dropped the request"),
            Self::Continue(msg) => write!(f, "unexpected Continue (kind: {})", msg.kind),
            Self::Malformed => write!(f, "stream ended without terminal event"),
        }
    }
}

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

impl OutputStream {
    /// Drains the stream, concatenates Chunk payloads into a body, accumulates
    /// Meta entries (mid-stream + trailing from Complete), and returns a
    /// `BufferedResponse` on success or a `TerminalNotResponse` on any non-Complete terminal.
    pub async fn collect_buffered(mut self) -> Result<BufferedResponse, TerminalNotResponse> {
        use futures::StreamExt;
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
                StreamEvent::Error(e) => return Err(TerminalNotResponse::Error(*e)),
                StreamEvent::Drop => return Err(TerminalNotResponse::Drop),
                StreamEvent::Continue(msg) => return Err(TerminalNotResponse::Continue(msg)),
            }
        }
        Err(TerminalNotResponse::Malformed)
    }

    /// View the body-carrying chunks as a `Stream<Item = Vec<u8>>`, filtering Meta
    /// events and stopping at the first terminal. Useful for piping one block's
    /// output into another block's InputStream.
    pub fn body_stream(self) -> impl Stream<Item = Vec<u8>> + Send + 'static {
        use futures::StreamExt;
        self.filter_map(|evt| async move {
            match evt {
                StreamEvent::Chunk(bytes) => Some(bytes),
                _ => None,
            }
        })
    }

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
                StreamEvent::Error(e) => Some(Err(*e)),
                StreamEvent::Meta(_) => None,
                StreamEvent::Complete { .. } | StreamEvent::Drop | StreamEvent::Continue(_) => None,
            }
        })
    }

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
        let cancel_clone = cancel;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_types::{Message, MetaEntry};
    use crate::stream::StreamEvent;
    use futures::StreamExt;

    #[tokio::test]
    async fn sink_send_chunk_then_complete() {
        let (mut rx, sink, _cancel) = new_streaming_channel(16);

        sink.send_chunk(b"hello".to_vec()).await.unwrap();
        sink.complete(vec![]).await.unwrap();

        let first = rx.recv().await.unwrap();
        assert_eq!(first, StreamEvent::Chunk(b"hello".to_vec()));

        let second = rx.recv().await.unwrap();
        assert!(matches!(second, StreamEvent::Complete { .. }));

        assert!(
            rx.recv().await.is_none(),
            "channel should close after terminal"
        );
    }

    #[tokio::test]
    async fn sink_send_chunk_returns_err_when_consumer_dropped() {
        let (rx, sink, _cancel) = new_streaming_channel(16);
        drop(rx);
        let err = sink.send_chunk(b"x".to_vec()).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn sink_send_meta_flows() {
        let (mut rx, sink, _cancel) = new_streaming_channel(16);
        let entry = MetaEntry {
            key: "Content-Type".into(),
            value: "text/event-stream".into(),
        };
        sink.send_meta(entry.clone()).await.unwrap();
        sink.complete(vec![]).await.unwrap();

        assert_eq!(rx.recv().await.unwrap(), StreamEvent::Meta(entry));
        assert!(matches!(
            rx.recv().await.unwrap(),
            StreamEvent::Complete { .. }
        ));
    }

    #[tokio::test]
    async fn sink_error_terminal() {
        let err = crate::core_types::WaferError {
            code: crate::core_types::ErrorCode::Unknown,
            message: "boom".into(),
            meta: vec![],
        };
        let (mut rx, sink, _cancel) = new_streaming_channel(16);
        sink.error(err.clone()).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), StreamEvent::Error(Box::new(err)));
    }

    #[tokio::test]
    async fn sink_drop_terminal() {
        let (mut rx, sink, _cancel) = new_streaming_channel(16);
        sink.drop_request().await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), StreamEvent::Drop);
    }

    #[tokio::test]
    async fn sink_continue_terminal() {
        let (mut rx, sink, _cancel) = new_streaming_channel(16);
        let msg = Message {
            kind: "next".into(),
            meta: vec![],
        };
        sink.continue_with(msg.clone()).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), StreamEvent::Continue(msg));
    }

    #[tokio::test]
    async fn respond_is_single_chunk_plus_complete() {
        let stream = OutputStream::respond(b"hello".to_vec());
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], StreamEvent::Chunk(b"hello".to_vec()));
        assert!(matches!(events[1], StreamEvent::Complete { .. }));
    }

    #[tokio::test]
    async fn respond_with_meta_includes_trailing_meta() {
        let meta = vec![MetaEntry {
            key: "Content-Type".into(),
            value: "text/html".into(),
        }];
        let stream = OutputStream::respond_with_meta(b"<h1>hi</h1>".to_vec(), meta.clone());
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], StreamEvent::Chunk(b"<h1>hi</h1>".to_vec()));
        assert_eq!(events[1], StreamEvent::Complete { meta });
    }

    #[tokio::test]
    async fn respond_with_meta_empty_body_skips_chunk() {
        let meta = vec![MetaEntry {
            key: "resp.status".into(),
            value: "204".into(),
        }];
        let stream = OutputStream::respond_with_meta(vec![], meta.clone());
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 1, "empty body should skip Chunk");
        assert_eq!(events[0], StreamEvent::Complete { meta });
    }

    #[tokio::test]
    async fn error_is_single_terminal() {
        let err = crate::core_types::WaferError {
            code: crate::core_types::ErrorCode::Unknown,
            message: "boom".into(),
            meta: vec![],
        };
        let stream = OutputStream::error(err.clone());
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], StreamEvent::Error(Box::new(err)));
    }

    #[tokio::test]
    async fn drop_request_is_single_terminal() {
        let stream = OutputStream::drop_request();
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], StreamEvent::Drop);
    }

    #[tokio::test]
    async fn continue_with_is_single_terminal() {
        let msg = Message {
            kind: "next".into(),
            meta: vec![],
        };
        let stream = OutputStream::continue_with(msg.clone());
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], StreamEvent::Continue(msg));
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

    #[tokio::test]
    async fn dropping_stream_cancels_paired_token() {
        let (stream, _sink, cancel) = OutputStream::new_streaming();
        let observer = cancel;
        assert!(!observer.is_cancelled());
        drop(stream);
        assert!(
            observer.is_cancelled(),
            "dropping OutputStream should cancel paired token"
        );
    }

    #[tokio::test]
    async fn new_streaming_with_capacity_applies() {
        let (stream, sink, _cancel) = OutputStream::new_streaming_with_capacity(1);
        // Fill the buffer (capacity 1) — a second send should block.
        sink.send_chunk(b"a".to_vec()).await.unwrap();
        // Don't assert blocking here (hard to time-sensitive-test) — just confirm
        // that send + drain still works with non-default capacity.
        drop(sink);
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 1);
    }

    #[tokio::test]
    async fn collect_buffered_concatenates_chunks() {
        let (stream, sink, _cancel) = OutputStream::new_streaming();
        let entry = MetaEntry {
            key: "X-Final".into(),
            value: "1".into(),
        };
        let entry_clone = entry.clone();
        tokio::spawn(async move {
            sink.send_chunk(b"he".to_vec()).await.unwrap();
            sink.send_chunk(b"llo".to_vec()).await.unwrap();
            sink.complete(vec![entry_clone]).await.unwrap();
        });
        let buf = stream.collect_buffered().await.unwrap();
        assert_eq!(buf.body, b"hello");
        assert_eq!(buf.meta.len(), 1);
        assert_eq!(buf.meta[0], entry);
    }

    #[tokio::test]
    async fn collect_buffered_errors_on_error_terminal() {
        let err = crate::core_types::WaferError {
            code: crate::core_types::ErrorCode::Unknown,
            message: "oops".into(),
            meta: vec![],
        };
        let stream = OutputStream::error(err);
        let result = stream.collect_buffered().await;
        assert!(matches!(result, Err(TerminalNotResponse::Error(_))));
    }

    #[tokio::test]
    async fn collect_buffered_errors_on_drop_terminal() {
        let stream = OutputStream::drop_request();
        let result = stream.collect_buffered().await;
        assert!(matches!(result, Err(TerminalNotResponse::Drop)));
    }

    #[tokio::test]
    async fn collect_buffered_errors_on_continue_terminal() {
        let msg = Message {
            kind: "next".into(),
            meta: vec![],
        };
        let stream = OutputStream::continue_with(msg);
        let result = stream.collect_buffered().await;
        assert!(matches!(result, Err(TerminalNotResponse::Continue(_))));
    }

    #[tokio::test]
    async fn collect_buffered_collects_mid_stream_meta_before_complete() {
        let (stream, sink, _cancel) = OutputStream::new_streaming();
        let mid = MetaEntry {
            key: "X-Progress".into(),
            value: "50".into(),
        };
        let trailing = MetaEntry {
            key: "X-Final".into(),
            value: "1".into(),
        };
        let mid_clone = mid.clone();
        let trailing_clone = trailing.clone();
        tokio::spawn(async move {
            sink.send_meta(mid_clone).await.unwrap();
            sink.send_chunk(b"data".to_vec()).await.unwrap();
            sink.complete(vec![trailing_clone]).await.unwrap();
        });
        let buf = stream.collect_buffered().await.unwrap();
        assert_eq!(buf.body, b"data");
        assert_eq!(buf.meta, vec![mid, trailing]);
    }

    #[tokio::test]
    async fn body_stream_yields_only_chunks() {
        let (stream, sink, _cancel) = OutputStream::new_streaming();
        tokio::spawn(async move {
            sink.send_meta(MetaEntry {
                key: "k".into(),
                value: "v".into(),
            })
            .await
            .unwrap();
            sink.send_chunk(b"abc".to_vec()).await.unwrap();
            sink.send_chunk(b"def".to_vec()).await.unwrap();
            sink.complete(vec![]).await.unwrap();
        });
        let chunks: Vec<Vec<u8>> = stream.body_stream().collect().await;
        assert_eq!(chunks, vec![b"abc".to_vec(), b"def".to_vec()]);
    }

    #[tokio::test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "Drop terminal cannot follow")]
    async fn sink_drop_request_panics_after_chunk() {
        let (_rx, sink, _cancel) = new_streaming_channel(16);
        sink.send_chunk(b"x".to_vec()).await.unwrap();
        let _ = sink.drop_request().await;
    }

    #[tokio::test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "Drop terminal cannot follow")]
    async fn sink_drop_request_panics_after_meta() {
        let (_rx, sink, _cancel) = new_streaming_channel(16);
        sink.send_meta(MetaEntry {
            key: "k".into(),
            value: "v".into(),
        })
        .await
        .unwrap();
        let _ = sink.drop_request().await;
    }

    #[tokio::test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "Continue terminal cannot follow")]
    async fn sink_continue_panics_after_chunk() {
        let (_rx, sink, _cancel) = new_streaming_channel(16);
        sink.send_chunk(b"x".to_vec()).await.unwrap();
        let _ = sink
            .continue_with(Message {
                kind: "next".into(),
                meta: vec![],
            })
            .await;
    }

    #[tokio::test]
    async fn sink_drop_request_ok_with_no_prior_events() {
        let (_rx, sink, _cancel) = new_streaming_channel(16);
        sink.drop_request().await.unwrap(); // no panic
    }

    #[tokio::test]
    async fn sink_continue_ok_with_no_prior_events() {
        let (_rx, sink, _cancel) = new_streaming_channel(16);
        sink.continue_with(Message {
            kind: "next".into(),
            meta: vec![],
        })
        .await
        .unwrap();
    }

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
        assert_eq!(events[0], StreamEvent::Error(Box::new(err)));
    }

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
        assert_eq!(events[0], StreamEvent::Error(Box::new(err)));
    }
}
