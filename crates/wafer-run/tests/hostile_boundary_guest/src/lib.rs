//! Guest-boundary e2e fixture: an ordinary public-SDK guest that probes what
//! crosses the WASM guest boundary in each direction.
//!
//! Implements the standard `__wafer_handle(ptr, len) -> i64` ABI directly
//! (without the `#[wafer_block]` macro), like `hostile_db_guest`. Operations
//! are selected by `msg.kind`:
//!
//! - `"GET:/echo_meta"` (the kind the HTTP codec builds for `GET /echo_meta`)
//!   — respond with the meta the guest was handed, as a JSON object. Shows
//!   which request headers reached the guest.
//! - `"test.error_with_headers"` — return an `Error` whose meta sets a cookie,
//!   a redirect and a CORS header.
//! - `"test.continue_with_headers"` — hand the received message on as a
//!   `Continue` after adding the same response headers and forging the
//!   request's `Cookie` and `Authorization`.
//! - `"test.stream_init_flood"` — open streams with a 1 MiB message until the
//!   host refuses one (at most [`FLOOD_ATTEMPTS`]); respond with how many
//!   opened and the refusal's error code.
//!
//! Declared capabilities: it may READ `authorization` (and nothing else
//! sensitive), may WRITE no sensitive header, and may call only
//! [`SINK_BLOCK`] — which need not exist, since `stream_init` never dispatches.

use wafer_sdk::{
    core_abi::{pack_ptr_len, GuestResult},
    stream::CallStream,
    BlockInfo, ErrorCode, GuestAction, Message, MetaEntry, WaferError,
};

/// The only block the guest may open a stream to.
const SINK_BLOCK: &str = "test/sink";

/// Upper bound on streams the flood op opens.
const FLOOD_ATTEMPTS: usize = 64;

/// Padding carried by each flood message.
const FLOOD_MESSAGE_BYTES: usize = 1024 * 1024;

#[no_mangle]
pub extern "C" fn __wafer_info() -> i64 {
    let info = BlockInfo::new(
        "test/hostile-boundary-guest",
        "0.0.0",
        "handler@v1",
        "Guest-boundary e2e fixture",
    )
    .capabilities(wafer_sdk::BlockCapabilities {
        callable_blocks: wafer_sdk::Allowlist::Only(
            [SINK_BLOCK].into_iter().map(String::from).collect(),
        ),
        headers: wafer_sdk::capabilities::HeaderPolicy {
            readable: vec!["authorization".to_string()],
            ..Default::default()
        },
        ..wafer_sdk::BlockCapabilities::none()
    });
    leak_packed(serde_json::to_vec(&info).expect("BlockInfo is JSON-serialisable"))
}

#[no_mangle]
pub extern "C" fn __wafer_lifecycle(_evt_ptr: i32, _evt_len: i32) -> i64 {
    leak_packed(
        serde_json::to_vec(&Ok::<(), WaferError>(()))
            .expect("Result<(), WaferError>::Ok(()) is JSON-serialisable"),
    )
}

#[no_mangle]
pub extern "C" fn __wafer_handle(msg_ptr: i32, msg_len: i32) -> i64 {
    let msg_bytes = unsafe { std::slice::from_raw_parts(msg_ptr as *const u8, msg_len as usize) };
    let result = match serde_json::from_slice::<(Message, Vec<u8>)>(msg_bytes) {
        Ok((msg, _body)) => dispatch(msg),
        Err(e) => GuestResult::error(WaferError::new(
            ErrorCode::InvalidArgument,
            format!("hostile-boundary-guest: invalid (Message, body) tuple: {e}"),
        )),
    };
    leak_packed(serde_json::to_vec(&result).expect("GuestResult is always JSON-serialisable"))
}

fn leak_packed(bytes: Vec<u8>) -> i64 {
    let ptr = bytes.as_ptr() as u32;
    let len = bytes.len() as u32;
    std::mem::forget(bytes);
    pack_ptr_len(ptr, len)
}

fn entry(key: &str, value: &str) -> MetaEntry {
    MetaEntry {
        key: key.to_string(),
        value: value.to_string(),
    }
}

/// Response headers no guest in this fixture may write.
fn hostile_response_headers() -> Vec<MetaEntry> {
    vec![
        entry("resp.set_cookie.s", "s=evil; Path=/"),
        entry("resp.header.location", "https://evil.example/"),
        entry("resp.header.access-control-allow-origin", "*"),
        // Every header the security-headers middleware sets: a guest without
        // a `writable` grant must not replace one.
        entry("resp.header.strict-transport-security", "max-age=0"),
        entry("resp.header.x-frame-options", "ALLOWALL"),
        entry("resp.header.content-security-policy", "script-src *"),
        entry("resp.header.x-content-type-options", "sniff"),
        entry("resp.header.Referrer-Policy", "unsafe-url"),
        entry("resp.header.permissions-policy", "camera=*"),
        entry("resp.header.cross-origin-opener-policy", "unsafe-none"),
        entry("resp.header.cross-origin-embedder-policy", "unsafe-none"),
        entry("resp.header.x-guest", "kept"),
    ]
}

fn dispatch(msg: Message) -> GuestResult {
    match msg.kind.as_str() {
        "GET:/echo_meta" => {
            let seen: serde_json::Map<String, serde_json::Value> = msg
                .meta
                .iter()
                .map(|e| (e.key.clone(), serde_json::Value::String(e.value.clone())))
                .collect();
            GuestResult::respond(serde_json::to_vec(&seen).expect("a JSON map serialises"))
        }
        "test.error_with_headers" => {
            let mut err = WaferError::new(ErrorCode::InvalidArgument, "refused");
            err.meta = hostile_response_headers();
            GuestResult::error(err)
        }
        "test.continue_with_headers" => {
            let mut next = msg;
            next.meta
                .retain(|e| e.key != "http.header.cookie" && e.key != "http.header.authorization");
            next.meta.extend(hostile_response_headers());
            next.meta.push(entry("http.header.cookie", "s=forged"));
            next.meta
                .push(entry("http.header.authorization", "Bearer forged"));
            GuestResult {
                action: GuestAction::Continue,
                response: None,
                error: None,
                message: Some(next),
            }
        }
        "test.stream_init_flood" => stream_init_flood(),
        other => GuestResult::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("hostile-boundary-guest: unknown kind {other}"),
        )),
    }
}

/// Open streams (kept alive, never finished) with a 1 MiB message each until
/// the host refuses one.
fn stream_init_flood() -> GuestResult {
    let mut big = Message::new("flood");
    big.meta
        .push(entry("pad", &"x".repeat(FLOOD_MESSAGE_BYTES)));
    let mut open = Vec::new();
    let mut refused: Option<String> = None;
    for _ in 0..FLOOD_ATTEMPTS {
        match CallStream::open(SINK_BLOCK, &big) {
            Ok(stream) => open.push(stream),
            Err(e) => {
                refused = Some(format!("{:?}", e.code));
                break;
            }
        }
    }
    let report = serde_json::json!({ "opened": open.len(), "refused": refused });
    GuestResult::respond(serde_json::to_vec(&report).expect("a JSON value serialises"))
}
