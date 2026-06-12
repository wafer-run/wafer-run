//! Internal helpers shared by typed-client modules.
//!
//! Each helper assumes the streaming ABI's frame-protocol invariants
//! (zero-or-more body frames after `finish`). Visibility is `pub(super)`
//! so only sibling client modules under [`super`] can call them; the
//! module itself is private to `clients/`.
//!
//! The three `call*` helpers cover the buffered op shapes shared by every
//! typed client:
//!
//! - [`call`] — encode a typed request, send it as one frame, decode a
//!   single typed response frame.
//! - [`call_ack`] — same request side, but the response is an empty
//!   acknowledgement that is drained and discarded.
//! - [`call_no_body`] — no request body at all; parameters (if any) travel
//!   in `Message::meta` headers, and a single typed response frame is
//!   decoded.
//!
//! Ops with bespoke framing (header-then-body responses, chunk streams)
//! build on [`open_buffered`] and [`decode_frame`] directly.

use serde::{de::DeserializeOwned, Serialize};
use wafer_block::{codec, ErrorCode, Message, MetaEntry, WaferError};

use crate::stream::{CallStream, ResponseStream};

/// Buffered request/response call: encode `request`, send it as a single
/// frame to `block`'s `op`, then decode the single response frame.
///
/// Error context strings are derived from `op` itself (e.g.
/// `"decoding database.get response: ..."`).
pub(super) fn call<Req, Resp>(block: &str, op: &str, request: &Req) -> Result<Resp, WaferError>
where
    Req: Serialize + ?Sized,
    Resp: DeserializeOwned,
{
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(block, op, &req_bytes)?;
    let body = collect_single_frame(&mut response_stream, op)?;
    decode_frame(&body, &format!("{op} response"))
}

/// Buffered call whose response is an empty acknowledgement: encode
/// `request`, send it as a single frame to `block`'s `op`, then drain the
/// ack frame(s).
pub(super) fn call_ack<Req>(block: &str, op: &str, request: &Req) -> Result<(), WaferError>
where
    Req: Serialize + ?Sized,
{
    let req_bytes = codec::encode(request)?;
    let mut response_stream = open_buffered(block, op, &req_bytes)?;
    consume_ack(&mut response_stream)
}

/// Call an op that carries no request body, decoding the single response
/// frame.
///
/// Parameters, if any, travel in `Message::meta` headers rather than the
/// body (e.g. the auth `require_*` ops, which read scope/role from
/// `http.header.x-auth-scope` / `http.header.x-auth-role`). Ops without
/// parameters pass an empty `meta`.
pub(super) fn call_no_body<Resp>(
    block: &str,
    op: &str,
    meta: Vec<MetaEntry>,
) -> Result<Resp, WaferError>
where
    Resp: DeserializeOwned,
{
    let msg = Message {
        kind: op.to_string(),
        meta,
    };
    let call = CallStream::open(block, &msg)?;
    let mut response_stream = call.finish()?;
    let body = collect_single_frame(&mut response_stream, op)?;
    decode_frame(&body, &format!("{op} response"))
}

/// Send a single-frame request to `block`'s op and return the response
/// stream.
///
/// Opens a [`CallStream`], writes one request frame, then `finish`es to flip
/// into receive mode. Used directly by ops with bespoke response framing
/// (e.g. storage `get`'s header-then-body, llm `chat`'s chunk stream).
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

/// Decode an encoded frame, prefixing decode errors with `context` so the
/// failing operation is identifiable (`"decoding {context}: ..."`).
pub(super) fn decode_frame<Resp>(body: &[u8], context: &str) -> Result<Resp, WaferError>
where
    Resp: DeserializeOwned,
{
    codec::decode(body)
        .map_err(|e| WaferError::new(e.code, format!("decoding {context}: {}", e.message)))
}

/// Drain a single-frame ack response.
///
/// Pulls and discards the ack frame (handlers may choose to send an empty
/// body), then drains any further frames defensively. Be tolerant here —
/// the stream's `Drop` will close the handle either way.
fn consume_ack(response_stream: &mut ResponseStream) -> Result<(), WaferError> {
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
/// error-message prefix; the `call*` helpers pass the op name itself
/// (e.g. `"storage.list"`).
fn collect_single_frame(
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
