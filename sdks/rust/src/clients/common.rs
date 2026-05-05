//! Internal helpers shared by typed-client modules.
//!
//! Each helper assumes the streaming ABI's frame-protocol invariants
//! (zero-or-more body frames after `finish`). Visibility is `pub(super)`
//! so only sibling client modules under [`super`] can call them; the
//! module itself is private to `clients/`.

use wafer_block::{ErrorCode, Message, WaferError};

use crate::stream::{CallStream, ResponseStream};

/// Send a single-frame request to `block`'s op and return the response stream.
///
/// Opens a [`CallStream`], writes one request frame, then `finish`es to flip
/// into receive mode. Use this for the common single-frame-request shape.
pub(super) fn open_buffered(
    block: &str,
    op: &str,
    req_bytes: &[u8],
) -> Result<ResponseStream, WaferError> {
    let msg = Message {
        kind: op.to_string(),
        meta: vec![],
    };
    let mut call = CallStream::open(block, &msg)?;
    call.write_chunk(req_bytes)?;
    call.finish()
}

/// Drain a single-frame ack response.
///
/// Pulls and discards the ack frame (handlers may choose to send an empty
/// body), then drains any further frames defensively. Be tolerant here —
/// the stream's `Drop` will close the handle either way.
pub(super) fn consume_ack(response_stream: &mut ResponseStream) -> Result<(), WaferError> {
    // Pull and discard the ack frame, if any.
    let _ = response_stream.next_chunk()?;
    // Drain any further frames (handlers should not emit them, but be
    // tolerant here — the stream's Drop will close the handle either way).
    while response_stream.next_chunk()?.is_some() {}
    Ok(())
}

/// Collect a single response frame whose payload carries an encoded value.
///
/// Errors if the stream ended without a frame. `context` is used as the
/// error-message prefix and should follow the `"<service> <OP_NAME>"`
/// convention (e.g. `"storage LIST"`).
pub(super) fn collect_single_frame(
    response_stream: &mut ResponseStream,
    context: &str,
) -> Result<Vec<u8>, WaferError> {
    let body = response_stream.next_chunk()?.ok_or_else(|| {
        WaferError::new(
            ErrorCode::Internal,
            format!("{context}: stream ended before response frame"),
        )
    })?;
    // Drain any trailing frames defensively.
    while response_stream.next_chunk()?.is_some() {}
    Ok(body)
}
