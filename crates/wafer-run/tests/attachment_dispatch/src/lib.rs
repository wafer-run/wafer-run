//! Test guest exercising the attach + lookup_attachment ABI end-to-end.
//!
//! Two operations selected by `msg.kind`:
//!   - `"test.attach_then_call"` — opens a streaming call to its own block
//!     (kind="test.echo_attachment"), attaches a fixed `("call_1", Attachment)`
//!     pair via [`CallStream::attach`], finishes the request with an empty
//!     body, drains the response stream, and returns the concatenated bytes
//!     the callee echoed back.
//!   - `"test.echo_attachment"` — calls [`wafer_sdk::lookup_attachment`] for
//!     id `"call_1"` and returns the attachment's bytes as the response body.
//!
//! Together they prove the full chain works: caller-side attach → host stream
//! ABI → host-side dispatch with attachments → callee-side lookup_attachment
//! → bytes round-tripped to the original caller.

use wafer_sdk::core_abi::{pack_ptr_len, GuestResult};
use wafer_sdk::stream::CallStream;
use wafer_sdk::{Attachment, BlockInfo, ErrorCode, Message, WaferError};

/// Block metadata export. Returns a JSON-encoded `BlockInfo` packed as
/// `(ptr << 32) | len` — the format `WasmiBlock::info()` expects.
#[no_mangle]
pub extern "C" fn __wafer_info() -> i64 {
    let info = BlockInfo::new(
        "test/attachment-dispatch-guest",
        "0.0.0",
        "handler@v1",
        "Exercises attach + lookup_attachment ABI",
    );
    let bytes = serde_json::to_vec(&info).expect("BlockInfo is JSON-serialisable");
    let ptr = bytes.as_ptr() as u32;
    let len = bytes.len() as u32;
    std::mem::forget(bytes);
    pack_ptr_len(ptr, len)
}

/// Lifecycle hook — no-op for this fixture. Returns `Result<(), WaferError>::Ok(())`.
#[no_mangle]
pub extern "C" fn __wafer_lifecycle(_evt_ptr: i32, _evt_len: i32) -> i64 {
    let bytes = serde_json::to_vec(&Ok::<(), WaferError>(()))
        .expect("Result<(), WaferError>::Ok(()) is JSON-serialisable");
    let ptr = bytes.as_ptr() as u32;
    let len = bytes.len() as u32;
    std::mem::forget(bytes);
    pack_ptr_len(ptr, len)
}

/// Standard wafer block handler entry point.
#[no_mangle]
pub extern "C" fn __wafer_handle(msg_ptr: i32, msg_len: i32) -> i64 {
    let msg_bytes =
        unsafe { std::slice::from_raw_parts(msg_ptr as *const u8, msg_len as usize) };
    let result = match serde_json::from_slice::<(Message, Vec<u8>)>(msg_bytes) {
        Ok((msg, _body)) => dispatch(&msg),
        Err(e) => GuestResult::error(WaferError::new(
            ErrorCode::InvalidArgument,
            format!("attachment-dispatch-guest: invalid (Message, body) tuple: {e}"),
        )),
    };
    let result_bytes =
        serde_json::to_vec(&result).expect("GuestResult is always JSON-serialisable");
    let ptr = result_bytes.as_ptr() as u32;
    let len = result_bytes.len() as u32;
    std::mem::forget(result_bytes);
    pack_ptr_len(ptr, len)
}

fn dispatch(msg: &Message) -> GuestResult {
    match msg.kind.as_str() {
        "test.attach_then_call" => attach_then_call(),
        "test.echo_attachment" => echo_attachment(),
        other => GuestResult::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("attachment-dispatch-guest: unknown kind {other}"),
        )),
    }
}

/// Open a streaming call to ourselves under `test.echo_attachment`, attach
/// a fixed payload, finish, drain the response, return the concatenated bytes.
fn attach_then_call() -> GuestResult {
    let target = "test/attachment-dispatch-guest";
    let inner_msg = Message::new("test.echo_attachment");

    let stream = match CallStream::open(target, &inner_msg) {
        Ok(s) => s,
        Err(e) => return GuestResult::error(e),
    };

    let att = Attachment {
        mime: "image/png".into(),
        bytes: vec![0xAA, 0xBB, 0xCC, 0xDD],
        filename: Some("a.png".into()),
    };
    if let Err(e) = stream.attach("call_1", att) {
        return GuestResult::error(e);
    }

    // Empty request body — the callee uses lookup_attachment, not the body.
    let mut response = match stream.finish() {
        Ok(r) => r,
        Err(e) => return GuestResult::error(e),
    };

    let mut full = Vec::new();
    loop {
        match response.next_chunk() {
            Ok(Some(chunk)) => full.extend_from_slice(&chunk),
            Ok(None) => break,
            Err(e) => return GuestResult::error(e),
        }
    }
    GuestResult::respond(full)
}

/// Pull the inbound attachment under id `"call_1"` and echo its bytes back.
fn echo_attachment() -> GuestResult {
    match wafer_sdk::lookup_attachment("call_1") {
        Ok(Some(att)) => GuestResult::respond(att.bytes),
        Ok(None) => GuestResult::error(WaferError::new(
            ErrorCode::NotFound,
            "echo_attachment: no attachment for id 'call_1'",
        )),
        Err(e) => GuestResult::error(e),
    }
}
