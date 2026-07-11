//! Output side of a block invocation: an [`OutputStream`] consumer paired
//! with an [`OutputSink`] producer handle. The producer emits zero or more
//! [`StreamEvent::Chunk`]/[`StreamEvent::Meta`] events followed by exactly
//! one terminal event (`Complete`/`Error`/`Drop`/`Continue`/`Halt`).

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures::stream::Stream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::{
    core_types::{Message, MetaEntry, WaferError},
    stream::StreamEvent,
};

/// Signals that the consumer dropped the stream.
#[derive(Debug, thiserror::Error)]
#[error("output sink closed: consumer dropped")]
pub struct SinkClosed;

/// Error returned by the body-free terminal methods
/// ([`OutputSink::drop_request`], [`OutputSink::continue_with`]) when they
/// cannot be applied.
///
/// These terminals signal "no response body" (Drop) or "forward elsewhere"
/// (Continue), so the protocol forbids them after any `Chunk`/`Meta` has
/// already been emitted on the sink. The invariant is enforced in both debug
/// and release builds: a violation refuses to send the terminal and returns
/// [`SinkSendError::BodyAlreadySent`] rather than corrupting the stream.
#[derive(Debug, thiserror::Error)]
pub enum SinkSendError {
    /// The consumer dropped the stream before the terminal could be sent.
    #[error("output sink closed: consumer dropped")]
    Closed,
    /// A `Chunk` or `Meta` event was already emitted on this sink, so a
    /// body-free terminal (`Drop`/`Continue`) would violate the stream
    /// protocol. The terminal was refused; no event was sent.
    #[error("protocol violation: {0} terminal cannot follow Chunk or Meta events")]
    BodyAlreadySent(&'static str),
}

impl From<SinkClosed> for SinkSendError {
    fn from(_: SinkClosed) -> Self {
        Self::Closed
    }
}

/// Producer handle paired with an OutputStream. The producing task holds this sink
/// and calls send_chunk / send_meta for non-terminal events, then exactly one of
/// complete / error / drop_request / continue_with as the terminal event.
///
/// Terminal delivery is guaranteed: construction reserves one dedicated channel
/// slot (an [`mpsc::OwnedPermit`]) for the terminal event, so both the explicit
/// terminal methods and the Drop auto-`Complete` safety net can always deliver
/// their terminal even when the body channel is full. Body sends
/// (`send_chunk`/`send_meta`) still see exactly the requested capacity of
/// backpressure; terminals never backpressure.
pub struct OutputSink {
    tx: mpsc::Sender<StreamEvent>,
    /// Channel slot reserved at construction for the single terminal event.
    /// `Some` until a terminal is sent; taken by the explicit terminal
    /// methods and, if still present, by `Drop`'s auto-`Complete` safety net
    /// (which therefore cannot double-send after an explicit terminal).
    terminal_permit: Option<mpsc::OwnedPermit<StreamEvent>>,
    any_body_sent: std::sync::atomic::AtomicBool,
}

impl OutputSink {
    /// Send the terminal event through the reserved permit.
    ///
    /// The slot was reserved at construction, so this cannot fail on a full
    /// channel — the historical race where a producer's final `send_chunk`
    /// filled the channel and the terminal was silently lost (surfacing as
    /// `TerminalNotResponse::Malformed` → a spurious 500) is structurally
    /// impossible. If the consumer already dropped the receiver, the event is
    /// discarded by the channel and `Err(SinkClosed)` is returned, matching
    /// the previous `tx.send(..).await` semantics.
    fn send_terminal(&mut self, event: StreamEvent) -> Result<(), SinkClosed> {
        let permit = self
            .terminal_permit
            .take()
            .expect("terminal methods consume `self`, so the permit is still present");
        let closed = self.tx.is_closed();
        let _sender = permit.send(event);
        if closed {
            Err(SinkClosed)
        } else {
            Ok(())
        }
    }

    /// Send a body chunk. Awaits when the channel is full (backpressure).
    /// Returns Err if the consumer has dropped the stream.
    pub async fn send_chunk(&self, bytes: Vec<u8>) -> Result<(), SinkClosed> {
        self.any_body_sent
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.tx
            .send(StreamEvent::Chunk(bytes))
            .await
            .map_err(|_| SinkClosed)
    }

    /// Send a mid-stream metadata event (e.g., Content-Type declaration).
    pub async fn send_meta(&self, entry: MetaEntry) -> Result<(), SinkClosed> {
        self.any_body_sent
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.tx
            .send(StreamEvent::Meta(entry))
            .await
            .map_err(|_| SinkClosed)
    }

    /// Terminal. Must be called exactly once per sink. Delivered via the
    /// reserved terminal slot, so it never blocks on a full body channel.
    pub async fn complete(mut self, meta: Vec<MetaEntry>) -> Result<(), SinkClosed> {
        self.send_terminal(StreamEvent::Complete { meta })
    }

    /// Terminal. The block encountered an error. Delivered via the reserved
    /// terminal slot, so it never blocks on a full body channel.
    pub async fn error(mut self, err: WaferError) -> Result<(), SinkClosed> {
        self.send_terminal(StreamEvent::Error(Box::new(err)))
    }

    /// Terminal. The block chose to drop the request (HTTP 204-equivalent).
    ///
    /// Refused if a `Chunk`/`Meta` was already emitted on this sink: a `Drop`
    /// carries no body, so following body events with it is a protocol
    /// violation. In that case no event is sent and
    /// [`SinkSendError::BodyAlreadySent`] is returned.
    pub async fn drop_request(mut self) -> Result<(), SinkSendError> {
        if self
            .any_body_sent
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            tracing::warn!("Drop terminal cannot follow Chunk or Meta events; refusing");
            return Err(SinkSendError::BodyAlreadySent("Drop"));
        }
        self.send_terminal(StreamEvent::Drop)
            .map_err(SinkSendError::from)
    }

    /// Terminal. Forward to another block instead of handling.
    ///
    /// Refused if a `Chunk`/`Meta` was already emitted on this sink: a
    /// `Continue` forwards the request elsewhere and carries no body, so
    /// following body events with it is a protocol violation. In that case no
    /// event is sent and [`SinkSendError::BodyAlreadySent`] is returned.
    pub async fn continue_with(mut self, msg: Message) -> Result<(), SinkSendError> {
        if self
            .any_body_sent
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            tracing::warn!("Continue terminal cannot follow Chunk or Meta events; refusing");
            return Err(SinkSendError::BodyAlreadySent("Continue"));
        }
        self.send_terminal(StreamEvent::Continue(msg))
            .map_err(SinkSendError::from)
    }

    /// Terminal. Block produced a response AND requests short-circuit.
    /// HTTP boundary serves the supplied body+meta; flow executor halts the
    /// step loop. The `body` parameter is the complete response body — do
    /// not mix Halt with prior streamed Chunk events on the same sink.
    pub async fn halt(
        mut self,
        body: Vec<u8>,
        meta: Vec<crate::core_types::MetaEntry>,
    ) -> Result<(), SinkClosed> {
        self.send_terminal(StreamEvent::Halt { body, meta })
    }
}

impl Drop for OutputSink {
    fn drop(&mut self) {
        // Safety net: a producer dropped the sink without an explicit
        // terminal. `from_producer` documents this as auto-`Complete`, so
        // we keep the consumer's stream terminating — but for any other
        // code path it usually means a forgotten terminal, so we warn to
        // surface the case. If a body was streamed first, this is an
        // empty-meta Complete, which is the intended close.
        //
        // The permit is `None` when an explicit terminal already consumed it,
        // so this can never double-send. Sending through the reserved permit
        // cannot fail on a full channel (Drop cannot `.await`, and
        // `OwnedPermit::send` does not await) — previously a lossy `try_send`
        // here silently dropped the auto-`Complete` when the body channel was
        // full, turning a successful stream into a race-dependent
        // `TerminalNotResponse::Malformed` → 500.
        if let Some(permit) = self.terminal_permit.take() {
            if self
                .any_body_sent
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                tracing::debug!("OutputSink dropped after Chunk/Meta without explicit terminal; auto-completing");
            } else {
                tracing::warn!(
                    "OutputSink dropped without any event or terminal; auto-completing (likely a forgotten terminal)"
                );
            }
            let _sender = permit.send(StreamEvent::Complete { meta: vec![] });
        }
    }
}

/// Internal constructor used by OutputStream::new_streaming.
///
/// `capacity` is the BODY capacity: the underlying channel is allocated with
/// one extra slot which is immediately reserved (as an [`mpsc::OwnedPermit`])
/// for the terminal event. Body sends therefore see exactly `capacity` slots
/// of backpressure, while the terminal can always be delivered — even when
/// every body slot is occupied at the moment the sink is dropped.
pub(crate) fn new_streaming_channel(
    capacity: usize,
) -> (mpsc::Receiver<StreamEvent>, OutputSink, CancellationToken) {
    assert!(capacity > 0, "OutputStream channel capacity must be >= 1");
    let (tx, rx) = mpsc::channel(capacity + 1);
    let terminal_permit = tx
        .clone()
        .try_reserve_owned()
        .expect("freshly-allocated channel always has the terminal slot free");
    let cancel = CancellationToken::new();
    let sink = OutputSink {
        tx,
        terminal_permit: Some(terminal_permit),
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
    /// Private helper: allocates a pre-filled channel from a fixed-size array of
    /// events and builds an `OutputStream`. The channel capacity equals `N` exactly —
    /// no heap allocation or `Vec` collect. Because the channel is freshly allocated,
    /// `try_send` always has capacity and the `expect` is a protocol assertion, not a
    /// runtime risk.
    fn from_events<const N: usize>(events: [StreamEvent; N]) -> Self {
        const { assert!(N >= 1, "from_events requires at least one event") }
        let (tx, rx) = mpsc::channel::<StreamEvent>(N);
        for ev in events {
            tx.try_send(ev)
                .expect("freshly-allocated channel always has capacity");
        }
        Self {
            rx: ReceiverStream::new(rx),
            cancel: CancellationToken::new(),
        }
    }

    /// Buffered helper: emits one Chunk then Complete with no trailing meta.
    ///
    /// Pre-fills the mpsc channel synchronously; no spawn needed.
    pub fn respond(bytes: Vec<u8>) -> Self {
        Self::from_events([
            StreamEvent::Chunk(bytes),
            StreamEvent::Complete { meta: vec![] },
        ])
    }

    /// Buffered helper: emits one Chunk (if non-empty) then Complete with the
    /// given trailing meta.
    pub fn respond_with_meta(bytes: Vec<u8>, meta: Vec<crate::core_types::MetaEntry>) -> Self {
        if bytes.is_empty() {
            Self::from_events([StreamEvent::Complete { meta }])
        } else {
            Self::from_events([StreamEvent::Chunk(bytes), StreamEvent::Complete { meta }])
        }
    }

    /// Buffered helper: emits a single Error terminal event.
    pub fn error(err: WaferError) -> Self {
        Self::from_events([StreamEvent::Error(Box::new(err))])
    }

    /// Buffered helper: emits a single Drop terminal event.
    pub fn drop_request() -> Self {
        Self::from_events([StreamEvent::Drop])
    }

    /// Buffered helper: emits a single Continue terminal event.
    pub fn continue_with(msg: Message) -> Self {
        Self::from_events([StreamEvent::Continue(msg)])
    }

    /// Buffered helper: emits a single `Halt` terminal event carrying the
    /// supplied body + meta. Use when a block needs to short-circuit a flow
    /// while still delivering a response to the HTTP boundary.
    pub fn halt(body: Vec<u8>, meta: Vec<crate::core_types::MetaEntry>) -> Self {
        Self::from_events([StreamEvent::Halt { body, meta }])
    }

    /// Rebuild an `OutputStream` from a `BufferedResponse`, emitting a single
    /// `Halt` terminal. Used by the flow executor to forward a halted step's
    /// response up to the outer flow / HTTP boundary while preserving the
    /// short-circuit signal.
    pub fn from_buffered_response(buf: BufferedResponse) -> Self {
        Self::from_events([StreamEvent::Halt {
            body: buf.body,
            meta: buf.meta,
        }])
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

    /// Test-only: build an `OutputStream` over a raw event channel, bypassing
    /// [`OutputSink`] entirely. Used to synthesize protocol-violating streams
    /// (e.g. ending without a terminal event) that the sink can no longer
    /// produce now that terminal delivery is guaranteed via a reserved permit
    /// — such streams can still reach consumers from non-sink sources like a
    /// buggy remote producer decoded off the wire.
    #[cfg(test)]
    pub(crate) fn from_raw_receiver(rx: mpsc::Receiver<StreamEvent>) -> Self {
        Self {
            rx: ReceiverStream::new(rx),
            cancel: CancellationToken::new(),
        }
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
    /// Concatenated body bytes from all `Chunk` events.
    pub body: Vec<u8>,
    /// Mid-stream `Meta` entries plus trailing meta from the `Complete`
    /// terminal, in stream order.
    pub meta: Vec<MetaEntry>,
}

/// Error returned by `OutputStream::collect_buffered` when the stream terminates
/// with something other than `Complete`.
#[derive(Debug)]
pub enum TerminalNotResponse {
    /// Stream ended with [`StreamEvent::Error`].
    Error(WaferError),
    /// Stream ended with [`StreamEvent::Drop`].
    Drop,
    /// Stream ended with [`StreamEvent::Halt`] — block produced a response
    /// AND requests short-circuit. Carries the buffered response.
    Halt(BufferedResponse),
    /// Stream ended with [`StreamEvent::Continue`].
    Continue(Message),
    /// Stream ended without emitting any terminal event (protocol violation).
    Malformed,
}

impl std::fmt::Display for TerminalNotResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Error(e) => write!(f, "block error: {e}"),
            Self::Drop => write!(f, "block dropped the request"),
            Self::Halt(buf) => write!(
                f,
                "block halted the flow ({} body bytes, {} meta entries)",
                buf.body.len(),
                buf.meta.len(),
            ),
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
            TerminalNotResponse::Halt(buf) => WaferError {
                code: crate::core_types::ErrorCode::Internal,
                message: format!(
                    "Halt terminal converted to error — bug: callers should match Halt before this conversion ({} body bytes, {} meta entries)",
                    buf.body.len(),
                    buf.meta.len(),
                ),
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
                StreamEvent::Halt {
                    body: halt_body,
                    meta: halt_meta,
                } => {
                    // Halt carries a complete response; any prior Chunk/Meta
                    // events are replaced by Halt's payload (per the sink doc
                    // contract — do not mix Halt with streamed chunks). If a
                    // producer mixed them anyway, those bytes are dropped here,
                    // which is a producer bug — surface it.
                    if !body.is_empty() || !meta.is_empty() {
                        tracing::warn!(
                            discarded_body_bytes = body.len(),
                            discarded_meta_entries = meta.len(),
                            "Halt terminal arrived after Chunk/Meta; discarding prior streamed events (producer must not mix Halt with chunks)"
                        );
                        debug_assert!(
                            body.is_empty() && meta.is_empty(),
                            "Halt terminal must not follow Chunk/Meta events"
                        );
                    }
                    return Err(TerminalNotResponse::Halt(BufferedResponse {
                        body: halt_body,
                        meta: halt_meta,
                    }));
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
                StreamEvent::Complete { .. }
                | StreamEvent::Drop
                | StreamEvent::Continue(_)
                | StreamEvent::Halt { .. } => None,
            }
        })
    }

    /// Creates a streaming `OutputStream` driven by a producer closure.
    ///
    /// The closure receives an [`OutputSink`] and a [`CancellationToken`]. It should
    /// call `sink.send_chunk()` / `sink.send_meta()` for non-terminal events. When
    /// the closure returns, the sink is dropped — if no terminal was explicitly sent
    /// (via `sink.complete()`, `sink.error()`, etc.), an auto-`Complete { meta: vec![] }`
    /// is emitted. The auto-`Complete` is delivered through a channel slot reserved
    /// at construction, so it cannot be lost even if the body channel is full at
    /// the moment the sink drops.
    ///
    /// For explicit error handling, call `sink.error(e).await` before returning.
    ///
    /// Platform-portable: uses `tokio::spawn` on native, `spawn_local` on wasm32 browser.
    /// Not available on wasm32-wasip1 (WASI) — use `GuestResult::respond` directly.
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

    /// Creates a streaming `OutputStream` driven by a producer closure (wasm32 browser variant).
    #[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
    pub fn from_producer<F, Fut>(f: F) -> Self
    where
        F: FnOnce(OutputSink, CancellationToken) -> Fut + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        let (stream, sink, cancel) = Self::new_streaming();
        crate::spawn::spawn_producer(async move {
            f(sink, cancel).await;
        });
        stream
    }

    /// WASI blocks (wasm32-wasip1) are synchronous — `from_producer` is not available.
    /// Use `GuestResult::respond(bytes)` to return a response directly.
    #[cfg(all(target_arch = "wasm32", target_os = "wasi"))]
    pub fn from_producer<F, Fut>(_f: F) -> Self
    where
        F: FnOnce(OutputSink, CancellationToken) -> Fut + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        panic!("OutputStream::from_producer is not supported on wasm32-wasip1 (WASI)");
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;
    use crate::{
        core_types::{Message, MetaEntry},
        stream::StreamEvent,
    };

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
        // Fill the body buffer (capacity 1) — a second send would block.
        sink.send_chunk(b"a".to_vec()).await.unwrap();
        // Don't assert blocking here (hard to time-sensitive-test) — just confirm
        // that send + drain still works with non-default capacity, and that the
        // drop-auto-Complete terminal is delivered even though the body slot is
        // full (the terminal has its own reserved slot).
        drop(sink);
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 2, "Chunk + auto-Complete terminal");
        assert_eq!(events[0], StreamEvent::Chunk(b"a".to_vec()));
        assert!(matches!(events[1], StreamEvent::Complete { .. }));
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
    async fn sink_drop_request_refused_after_chunk() {
        let (mut rx, sink, _cancel) = new_streaming_channel(16);
        sink.send_chunk(b"x".to_vec()).await.unwrap();
        let err = sink.drop_request().await;
        assert!(
            matches!(err, Err(SinkSendError::BodyAlreadySent("Drop"))),
            "Drop after a Chunk must be refused in all build profiles, got: {err:?}"
        );
        // The Chunk flowed; no Drop event was emitted. When the refused sink is
        // dropped, the safety-net auto-Complete closes the stream.
        assert_eq!(rx.recv().await.unwrap(), StreamEvent::Chunk(b"x".to_vec()));
        assert!(matches!(
            rx.recv().await.unwrap(),
            StreamEvent::Complete { .. }
        ));
    }

    #[tokio::test]
    async fn sink_drop_request_refused_after_meta() {
        let (_rx, sink, _cancel) = new_streaming_channel(16);
        sink.send_meta(MetaEntry {
            key: "k".into(),
            value: "v".into(),
        })
        .await
        .unwrap();
        let err = sink.drop_request().await;
        assert!(
            matches!(err, Err(SinkSendError::BodyAlreadySent("Drop"))),
            "Drop after a Meta must be refused in all build profiles, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn sink_continue_refused_after_chunk() {
        let (_rx, sink, _cancel) = new_streaming_channel(16);
        sink.send_chunk(b"x".to_vec()).await.unwrap();
        let err = sink
            .continue_with(Message {
                kind: "next".into(),
                meta: vec![],
            })
            .await;
        assert!(
            matches!(err, Err(SinkSendError::BodyAlreadySent("Continue"))),
            "Continue after a Chunk must be refused in all build profiles, got: {err:?}"
        );
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
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };

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
    async fn drop_delivers_terminal_even_when_channel_is_full() {
        // Capacity 1: the sole body slot is filled by send_chunk, forcing
        // the channel to be exactly full at the moment the sink is dropped
        // without an explicit terminal. This is the real race from the bug
        // report — `send_chunk` can return as soon as it occupies the last
        // free slot, so "channel full at drop" is not a contrived scenario.
        let (mut rx, sink, _cancel) = new_streaming_channel(1);
        sink.send_chunk(b"last".to_vec()).await.unwrap();
        drop(sink); // no explicit terminal — Drop's auto-Complete must still land

        let chunk = rx.recv().await.expect("chunk should have been delivered");
        assert_eq!(chunk, StreamEvent::Chunk(b"last".to_vec()));

        let terminal = rx.recv().await.expect(
            "a full channel at drop must still deliver a terminal event, not close silently \
             (consumer would otherwise see TerminalNotResponse::Malformed -> a race-dependent 500)",
        );
        assert!(
            matches!(terminal, StreamEvent::Complete { ref meta } if meta.is_empty()),
            "expected an auto-Complete terminal after Drop, got: {terminal:?}"
        );
        assert!(
            rx.recv().await.is_none(),
            "channel should close after the terminal"
        );
    }

    #[tokio::test]
    async fn explicit_terminal_still_delivers_when_body_channel_is_full() {
        // Same forced-full setup, but this time the producer calls an
        // explicit terminal instead of relying on Drop. The reserved permit
        // must protect this path too, not just the Drop safety net: without
        // it, a terminal queued behind body backpressure deadlocks here
        // (there is deliberately no concurrent consumer draining the
        // channel), so the send is bounded by a timeout to keep the failure
        // mode a clean assertion instead of a hung test.
        let (mut rx, sink, _cancel) = new_streaming_channel(1);
        sink.send_chunk(b"last".to_vec()).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), sink.complete(vec![]))
            .await
            .expect("explicit terminal must not block behind a full body channel")
            .unwrap();

        assert_eq!(
            rx.recv().await.unwrap(),
            StreamEvent::Chunk(b"last".to_vec())
        );
        assert!(matches!(
            rx.recv().await.unwrap(),
            StreamEvent::Complete { .. }
        ));
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

    #[tokio::test]
    async fn halt_emits_single_terminal_event() {
        use futures::StreamExt;
        let body = b"hello".to_vec();
        let meta = vec![MetaEntry {
            key: "resp.status".into(),
            value: "204".into(),
        }];
        let mut stream = OutputStream::halt(body.clone(), meta.clone());
        let evt = stream.next().await.expect("one event");
        assert_eq!(evt, StreamEvent::Halt { body, meta });
        assert!(
            stream.next().await.is_none(),
            "no more events after terminal"
        );
    }

    #[tokio::test]
    async fn collect_buffered_returns_halt_for_halt_event() {
        let body = b"abc".to_vec();
        let meta = vec![MetaEntry {
            key: "resp.header.X-Test".into(),
            value: "v".into(),
        }];
        let stream = OutputStream::halt(body.clone(), meta.clone());
        match stream.collect_buffered().await {
            Err(TerminalNotResponse::Halt(buf)) => {
                assert_eq!(buf.body, body);
                assert_eq!(buf.meta, meta);
            }
            other => panic!("expected Err(Halt), got {other:?}"),
        }
    }

    /// Build a Chunk-then-Halt stream by feeding the raw channel directly.
    /// `Halt` is a terminal carrying its own complete body, so the sink's
    /// terminal methods don't gate it — a producer can only get here by
    /// pre-sending Chunk/Meta, which is the protocol violation we want to
    /// surface in `collect_buffered`.
    fn chunk_then_halt_stream() -> OutputStream {
        let (tx, rx) = mpsc::channel::<StreamEvent>(4);
        tx.try_send(StreamEvent::Chunk(b"streamed".to_vec()))
            .unwrap();
        tx.try_send(StreamEvent::Halt {
            body: b"halt-body".to_vec(),
            meta: vec![MetaEntry {
                key: "resp.status".into(),
                value: "200".into(),
            }],
        })
        .unwrap();
        OutputStream {
            rx: ReceiverStream::new(rx),
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "Halt terminal must not follow Chunk/Meta events")]
    async fn collect_buffered_debug_asserts_on_halt_after_chunk() {
        let stream = chunk_then_halt_stream();
        let _ = stream.collect_buffered().await;
    }

    #[tokio::test]
    #[cfg(not(debug_assertions))]
    async fn collect_buffered_halt_after_chunk_discards_prior_in_release() {
        let stream = chunk_then_halt_stream();
        match stream.collect_buffered().await {
            Err(TerminalNotResponse::Halt(buf)) => {
                // Halt's payload wins; the prior streamed Chunk is discarded.
                assert_eq!(buf.body, b"halt-body");
                assert_eq!(buf.meta.len(), 1);
                assert_eq!(buf.meta[0].key, "resp.status");
            }
            other => panic!("expected Err(Halt), got {other:?}"),
        }
    }
}
