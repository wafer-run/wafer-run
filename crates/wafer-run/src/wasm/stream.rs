//! Streaming call lifecycle for the wasmi host: init → write_chunk* →
//! finish → read_chunk* → close.
//!
//! Each guest-initiated streaming call gets a [`StreamState`]. The host hands
//! the guest an `i64` cookie (the handle) returned by
//! `__wafer_host_stream_init`; subsequent host imports look up the state via
//! [`StreamRegistry`].
//!
//! Phase transitions are enforced — write_chunk after finish, read_chunk
//! before finish, etc. all return `WaferError(FailedPrecondition)`.
//!
//! Drop semantics: the registry's `Drop` impl closes any orphan streams when
//! the wasmi `Store` is dropped, which cancels in-flight `OutputStream`s via
//! their paired `CancellationToken`.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
};

use wafer_block::{
    streams::output::{OutputStream, TerminalNotResponse},
    ErrorCode, Message, WaferError,
};

/// State machine phase. The legal transitions are
/// `WritingRequest -> Finishing -> ReadingResponse -> Closed`. `Closed` is
/// terminal. `Finishing` is the brief window between `take_finish_request`
/// (which drains the request body) and either `finish_with_stream` (success)
/// or `record_error_and_close` (failure).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    WritingRequest,
    /// Request data has been drained; awaiting `finish_with_stream` or
    /// `record_error_and_close` from the resume loop.
    Finishing,
    ReadingResponse,
    Closed,
}

/// Per-stream state. Owned by [`StreamRegistry`].
pub(crate) struct StreamState {
    /// Block name to dispatch the call to. Only meaningful pre-finish.
    target_block: String,
    /// Message header for the call. Only meaningful pre-finish.
    msg: Option<Message>,
    /// Accumulated request body bytes from `write_chunk`.
    request_buffer: Vec<u8>,
    /// Caller-attached named binary blobs. Drained into the callee's
    /// `WasmiHostState::current_attachments` at dispatch time.
    attachments: std::collections::BTreeMap<String, wafer_block::Attachment>,
    /// The response stream installed by `finish_with_stream`. `next_chunk`
    /// drives this.
    response_stream: Option<OutputStream>,
    /// Most recent error captured for this stream (e.g. terminal Error frame
    /// from the response, or a precondition failure). Cleared by `take_error`.
    last_error: Option<WaferError>,
    phase: Phase,
}

impl StreamState {
    pub(crate) fn new(target_block: String, msg: Message) -> Self {
        Self {
            target_block,
            msg: Some(msg),
            request_buffer: Vec::new(),
            attachments: std::collections::BTreeMap::new(),
            response_stream: None,
            last_error: None,
            phase: Phase::WritingRequest,
        }
    }

    #[cfg(test)]
    pub(crate) fn phase(&self) -> Phase {
        self.phase
    }

    /// Append a request-body chunk. Only valid in `WritingRequest`.
    pub(crate) fn write_chunk(&mut self, chunk: &[u8]) -> Result<(), WaferError> {
        if self.phase != Phase::WritingRequest {
            return Err(precondition_err(
                "write_chunk called outside WritingRequest phase",
            ));
        }
        self.request_buffer.extend_from_slice(chunk);
        Ok(())
    }

    /// Add an attachment to the outgoing call. Only valid in `WritingRequest`.
    pub(crate) fn attach(
        &mut self,
        id: String,
        att: wafer_block::Attachment,
    ) -> Result<(), WaferError> {
        if self.phase != Phase::WritingRequest {
            return Err(precondition_err(
                "attach called outside WritingRequest phase",
            ));
        }
        self.attachments.insert(id, att);
        Ok(())
    }

    /// Remove and return the accumulated attachments. Called by the resume
    /// loop after `take_finish_request` to hand them to the callee.
    pub(crate) fn take_attachments(
        &mut self,
    ) -> std::collections::BTreeMap<String, wafer_block::Attachment> {
        std::mem::take(&mut self.attachments)
    }

    /// Consume the request side of the state machine, returning the data the
    /// resume loop needs to dispatch to `Context::call_block`. Transitions to
    /// a "finishing" intermediate — the caller is expected to install the
    /// resulting `OutputStream` via [`Self::finish_with_stream`] (on success)
    /// or [`Self::record_error_and_close`] (on capability denial / dispatch
    /// failure) before resuming the guest.
    pub(crate) fn take_finish_request(&mut self) -> Result<(String, Message, Vec<u8>), WaferError> {
        if self.phase != Phase::WritingRequest {
            return Err(precondition_err(
                "finish called outside WritingRequest phase",
            ));
        }
        let msg = self
            .msg
            .take()
            .ok_or_else(|| precondition_err("finish: message already consumed"))?;
        let body = std::mem::take(&mut self.request_buffer);
        self.phase = Phase::Finishing;
        Ok((self.target_block.clone(), msg, body))
    }

    /// Install the response stream produced by `Context::call_block` and
    /// transition to `ReadingResponse`. Mirrors the success path of `finish`.
    pub(crate) fn finish_with_stream(&mut self, stream: OutputStream) {
        debug_assert!(
            self.phase == Phase::Finishing,
            "finish_with_stream called in phase {:?}",
            self.phase,
        );
        self.response_stream = Some(stream);
        self.phase = Phase::ReadingResponse;
    }

    /// Pull the next chunk off the response stream.
    ///
    /// Returns:
    /// - `Ok(Some(bytes))` for a body chunk
    /// - `Ok(None)` for end-of-stream (Complete terminal)
    /// - `Err(WaferError)` for an error/drop/continue terminal — also stored
    ///   on `last_error` so `take_error` can return it.
    ///
    /// Mid-stream `Meta` events are absorbed silently (the streaming ABI does
    /// not currently surface them to the guest — adding a chunk-typed channel
    /// is a separate task).
    pub(crate) async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, WaferError> {
        if self.phase != Phase::ReadingResponse {
            return Err(precondition_err(
                "read_chunk called outside ReadingResponse phase",
            ));
        }
        let Some(stream) = self.response_stream.as_mut() else {
            return Err(precondition_err(
                "read_chunk: response stream missing (internal invariant)",
            ));
        };

        use futures::StreamExt;
        use wafer_block::stream::StreamEvent;

        loop {
            match stream.next().await {
                Some(StreamEvent::Chunk(bytes)) => return Ok(Some(bytes)),
                // Mid-stream / trailing meta entries are not yet surfaced to
                // the guest via this ABI. Skip them.
                Some(StreamEvent::Meta(_)) => continue,
                Some(StreamEvent::Complete { .. }) => {
                    // Drop the stream so its cancel token doesn't fire on a
                    // Complete-terminated channel (cosmetic, but tidy).
                    self.response_stream = None;
                    return Ok(None);
                }
                Some(StreamEvent::Error(boxed)) => {
                    let err = *boxed;
                    self.response_stream = None;
                    self.last_error = Some(err.clone());
                    return Err(err);
                }
                Some(StreamEvent::Drop) => {
                    self.response_stream = None;
                    let err =
                        WaferError::new(ErrorCode::ABORTED, "target block dropped the request");
                    self.last_error = Some(err.clone());
                    return Err(err);
                }
                Some(StreamEvent::Continue(next_msg)) => {
                    self.response_stream = None;
                    let err = WaferError::new(
                        ErrorCode::INTERNAL,
                        format!(
                            "unexpected Continue terminal (kind: {}) — call_block does not bridge Continue",
                            next_msg.kind,
                        ),
                    );
                    self.last_error = Some(err.clone());
                    return Err(err);
                }
                None => {
                    self.response_stream = None;
                    let err: WaferError = TerminalNotResponse::Malformed.into();
                    self.last_error = Some(err.clone());
                    return Err(err);
                }
            }
        }
    }

    /// Take the most recent error, if any. Subsequent calls return `None`
    /// until another error is captured.
    pub(crate) fn take_error(&mut self) -> Option<WaferError> {
        self.last_error.take()
    }

    /// Record an error captured by the host (e.g. capability denial when
    /// dispatching `finish`) and force the state into `Closed` so further
    /// imports report `FailedPrecondition`.
    pub(crate) fn record_error_and_close(&mut self, err: WaferError) {
        self.last_error = Some(err);
        self.response_stream = None;
        self.phase = Phase::Closed;
    }

    /// Close the stream. Cancels any in-flight response stream via the
    /// underlying `CancellationToken` (dropped here). Idempotent — calling
    /// `close` twice is a no-op.
    pub(crate) fn close(&mut self) {
        self.response_stream = None;
        self.request_buffer.clear();
        self.msg = None;
        // Preserve last_error so the guest can take it after close (if it
        // raced with an error).
        self.phase = Phase::Closed;
    }
}

impl Drop for StreamState {
    fn drop(&mut self) {
        // Dropping `response_stream` cancels its paired token; nothing more
        // is needed. The explicit close() call is for callers that want to
        // observe the post-close phase before drop.
    }
}

/// Build a `FailedPrecondition` error with a fixed prefix.
fn precondition_err(message: &str) -> WaferError {
    WaferError::new(
        ErrorCode::FAILED_PRECONDITION,
        format!("stream ABI misuse: {message}"),
    )
}

/// Per-instance map of live stream handles. One registry lives inside each
/// wasmi `Store`'s host state, so dropping the store cleans up orphan streams.
pub(crate) struct StreamRegistry {
    next_handle: AtomicU64,
    states: HashMap<u64, StreamState>,
}

impl StreamRegistry {
    pub(crate) fn new() -> Self {
        Self {
            // Start at 1 so 0 can stand for "no handle / sentinel".
            next_handle: AtomicU64::new(1),
            states: HashMap::new(),
        }
    }

    /// Allocate a fresh handle and store the state. Returns the handle.
    pub(crate) fn alloc(&mut self, state: StreamState) -> u64 {
        let h = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.states.insert(h, state);
        h
    }

    pub(crate) fn get_mut(&mut self, handle: u64) -> Option<&mut StreamState> {
        self.states.get_mut(&handle)
    }

    /// Drop the stream associated with `handle`. Idempotent — calling this on
    /// an unknown handle is a no-op.
    pub(crate) fn close(&mut self, handle: u64) {
        if let Some(mut state) = self.states.remove(&handle) {
            state.close();
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.states.len()
    }
}

impl Default for StreamRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for StreamRegistry {
    fn drop(&mut self) {
        // Drain so each StreamState's close() runs and its OutputStream's
        // CancellationToken fires for any orphan in-flight call.
        for (_, mut state) in self.states.drain() {
            state.close();
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use wafer_block::streams::output::OutputStream;

    use super::*;

    fn msg() -> Message {
        Message::new("test.kind")
    }

    #[tokio::test]
    async fn lifecycle_happy_path() {
        let mut state = StreamState::new("test/target".into(), msg());
        assert_eq!(state.phase(), Phase::WritingRequest);

        // write_chunk* in WritingRequest
        state.write_chunk(b"hello ").unwrap();
        state.write_chunk(b"world").unwrap();

        // finish: take request, install response stream
        let (target, _msg, body) = state.take_finish_request().unwrap();
        assert_eq!(target, "test/target");
        assert_eq!(body, b"hello world");

        let response = OutputStream::respond(b"OK".to_vec());
        state.finish_with_stream(response);
        assert_eq!(state.phase(), Phase::ReadingResponse);

        // read_chunk* in ReadingResponse
        let chunk = state.next_chunk().await.unwrap();
        assert_eq!(chunk.as_deref(), Some(b"OK".as_ref()));
        let eos = state.next_chunk().await.unwrap();
        assert!(eos.is_none(), "second read should be end-of-stream");

        // close
        state.close();
        assert_eq!(state.phase(), Phase::Closed);
    }

    #[tokio::test]
    async fn write_chunk_rejected_after_finish() {
        let mut state = StreamState::new("test/target".into(), msg());
        state.write_chunk(b"req").unwrap();
        let (_, _, _) = state.take_finish_request().unwrap();
        state.finish_with_stream(OutputStream::respond(vec![]));

        let err = state.write_chunk(b"late").unwrap_err();
        assert_eq!(err.code, ErrorCode::FailedPrecondition);
        assert!(err.message.contains("WritingRequest"), "{}", err.message);
    }

    #[tokio::test]
    async fn read_chunk_rejected_before_finish() {
        let mut state = StreamState::new("test/target".into(), msg());
        let err = state.next_chunk().await.unwrap_err();
        assert_eq!(err.code, ErrorCode::FailedPrecondition);
    }

    #[tokio::test]
    async fn finish_rejected_after_close() {
        let mut state = StreamState::new("test/target".into(), msg());
        state.close();
        let err = state.take_finish_request().unwrap_err();
        assert_eq!(err.code, ErrorCode::FailedPrecondition);
    }

    #[test]
    fn close_is_idempotent() {
        let mut state = StreamState::new("test/target".into(), msg());
        state.close();
        state.close();
        assert_eq!(state.phase(), Phase::Closed);
    }

    #[tokio::test]
    async fn error_terminal_captured_for_take_error() {
        let mut state = StreamState::new("test/target".into(), msg());
        let (_, _, _) = state.take_finish_request().unwrap();
        let err = WaferError::new(ErrorCode::INTERNAL, "boom");
        state.finish_with_stream(OutputStream::error(err.clone()));

        let res = state.next_chunk().await;
        assert!(res.is_err());
        let took = state.take_error().expect("error should be captured");
        assert_eq!(took.code, ErrorCode::Internal);
        assert_eq!(took.message, "boom");
        // Take is consuming.
        assert!(state.take_error().is_none());
    }

    #[tokio::test]
    async fn drop_terminal_yields_aborted_error() {
        let mut state = StreamState::new("test/target".into(), msg());
        let (_, _, _) = state.take_finish_request().unwrap();
        state.finish_with_stream(OutputStream::drop_request());

        let res = state.next_chunk().await;
        let err = res.unwrap_err();
        assert_eq!(err.code, ErrorCode::Aborted);
    }

    #[tokio::test]
    async fn meta_events_are_skipped() {
        use wafer_block::core_types::MetaEntry;
        let mut state = StreamState::new("test/target".into(), msg());
        let (_, _, _) = state.take_finish_request().unwrap();

        let meta = vec![MetaEntry {
            key: "X-Trailer".into(),
            value: "1".into(),
        }];
        state.finish_with_stream(OutputStream::respond_with_meta(b"hi".to_vec(), meta));

        let chunk = state.next_chunk().await.unwrap();
        assert_eq!(chunk.as_deref(), Some(b"hi".as_ref()));
        let eos = state.next_chunk().await.unwrap();
        assert!(eos.is_none());
    }

    #[test]
    fn registry_alloc_returns_unique_handles() {
        let mut reg = StreamRegistry::new();
        let h1 = reg.alloc(StreamState::new("a".into(), msg()));
        let h2 = reg.alloc(StreamState::new("b".into(), msg()));
        assert_ne!(h1, h2);
        assert!(h1 >= 1, "handle should be non-zero");
        assert!(reg.get_mut(h1).is_some());
        assert!(reg.get_mut(h2).is_some());
    }

    #[test]
    fn registry_close_removes_state() {
        let mut reg = StreamRegistry::new();
        let h = reg.alloc(StreamState::new("a".into(), msg()));
        assert_eq!(reg.len(), 1);
        reg.close(h);
        assert_eq!(reg.len(), 0);
        // Idempotent.
        reg.close(h);
        reg.close(99_999);
    }

    #[test]
    fn drop_cleans_up_orphans() {
        let mut reg = StreamRegistry::new();
        for _ in 0..5 {
            reg.alloc(StreamState::new("orphan".into(), msg()));
        }
        assert_eq!(reg.len(), 5);
        // Dropping should drain without panicking.
        drop(reg);
    }

    #[tokio::test]
    async fn record_error_and_close_transitions_to_closed() {
        let mut state = StreamState::new("test/target".into(), msg());
        let err = WaferError::new(ErrorCode::PERMISSION_DENIED, "no");
        state.record_error_and_close(err);
        assert_eq!(state.phase(), Phase::Closed);
        let took = state.take_error().expect("error should be present");
        assert_eq!(took.code, ErrorCode::PermissionDenied);
    }

    #[test]
    fn stream_state_attaches_and_drains() {
        use std::collections::BTreeMap;
        use wafer_block::Attachment;
        let mut state = StreamState::new("test/target".into(), msg());
        let att = Attachment {
            mime: "image/png".into(),
            bytes: vec![1, 2, 3],
            filename: None,
        };
        state.attach("call_1".into(), att.clone()).expect("attach");
        let drained: BTreeMap<String, Attachment> = state.take_attachments();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained.get("call_1"), Some(&att));
    }

    #[test]
    fn stream_state_attach_outside_writing_phase_errors() {
        use wafer_block::Attachment;
        let mut state = StreamState::new("test/target".into(), msg());
        // Force into a non-writing phase by taking finish.
        let _ = state.take_finish_request().expect("take_finish_request");
        let att = Attachment {
            mime: "x".into(),
            bytes: vec![],
            filename: None,
        };
        let r = state.attach("call_1".into(), att);
        assert!(r.is_err(), "attach must fail outside WritingRequest");
    }
}
