use std::sync::{Arc, Mutex};

use tracing::{debug, warn};
use wafer_block::streams::{input::InputStream, output::OutputStream};
use wafer_block_macro::wafer_async_trait;
use wasmi::{Caller, Engine, Error as WasmiError, Linker, Module, Store, TypedResumableCall, Val};

use super::{
    capabilities::BlockCapabilities,
    host::ContextGuard,
    stream::{StreamRegistry, StreamState},
};
use crate::{
    block::{Block, BlockInfo},
    context::Context,
    error::RuntimeError,
    types::*,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default fuel budget for a single guest invocation.
const DEFAULT_FUEL: u64 = 100_000_000;

/// Maximum WASM linear-memory pages (256 pages = 16 MiB).
const MAX_WASM_MEMORY_PAGES: usize = 256;

// ---------------------------------------------------------------------------
// Packed pointer helpers
// ---------------------------------------------------------------------------

/// Pack a (ptr, len) pair into a single i64 for the ABI.
fn pack_ptr_len(ptr: u32, len: u32) -> i64 {
    ((ptr as i64) << 32) | (len as i64)
}

/// Unpack a packed i64 into (ptr, len).
fn unpack_ptr_len(packed: i64) -> (u32, u32) {
    let ptr = (packed >> 32) as u32;
    let len = (packed & 0xFFFF_FFFF) as u32;
    (ptr, len)
}

// ---------------------------------------------------------------------------
// Guest meta sanitisation
// ---------------------------------------------------------------------------

/// Default set of HTTP header names that are considered security-sensitive.
/// WASM blocks cannot read or write these unless they declare them in their
/// `HeaderPolicy.readable` / `HeaderPolicy.writable` (and are granted the
/// cap after config intersection).
pub(crate) fn default_sensitive_headers() -> &'static [&'static str] {
    &[
        "authorization",
        "cookie",
        "set-cookie",
        "location",
        "access-control-allow-origin",
        "access-control-allow-credentials",
        "access-control-allow-methods",
        "access-control-allow-headers",
        "access-control-expose-headers",
        "access-control-max-age",
        "strict-transport-security",
        "x-frame-options",
        "content-security-policy",
        "content-security-policy-report-only",
    ]
}

fn is_sensitive_header(name: &str, policy_masked: &[String]) -> bool {
    let n = name.to_lowercase();
    default_sensitive_headers().contains(&n.as_str())
        || policy_masked.iter().any(|m| m.eq_ignore_ascii_case(&n))
}

/// Extract the canonical (lowercase) HTTP header name from a wafer meta key,
/// or `None` if the key is not a header.
///
/// Matches three forms:
/// - `req.header.{name}` — inbound request header
/// - `resp.header.{name}` — outbound response header
/// - `resp.set_cookie` / `resp.set_cookie.*` — legacy cookie keys, mapped to `set-cookie`
pub(crate) fn header_name_from_meta_key(key: &str) -> Option<String> {
    let lower = key.to_lowercase();
    if let Some(rest) = lower.strip_prefix("req.header.") {
        return Some(rest.to_string());
    }
    if let Some(rest) = lower.strip_prefix("resp.header.") {
        return Some(rest.to_string());
    }
    if lower == "resp.set_cookie" || lower.starts_with("resp.set_cookie.") {
        return Some("set-cookie".to_string());
    }
    None
}

/// Strip outbound meta entries whose header name is in the default sensitive
/// set plus `HeaderPolicy.masked`, unless explicitly in `HeaderPolicy.writable`.
/// Non-header meta entries pass through.
///
/// Stripped header names (deduped, lowercased) are appended to `stripped_names`.
pub(crate) fn sanitize_outbound_meta(
    meta: Vec<MetaEntry>,
    caps: &BlockCapabilities,
    stripped_names: &mut Vec<String>,
) -> Vec<MetaEntry> {
    meta.into_iter()
        .filter(|e| {
            let Some(name) = header_name_from_meta_key(&e.key) else {
                return true;
            };
            if !is_sensitive_header(&name, &caps.headers.masked) {
                return true;
            }
            let allowed = caps
                .headers
                .writable
                .iter()
                .any(|w| w.eq_ignore_ascii_case(&name));
            if !allowed {
                if !stripped_names.iter().any(|n| n == &name) {
                    stripped_names.push(name);
                }
                return false;
            }
            true
        })
        .collect()
}

/// Symmetric inbound sanitizer. Uses `HeaderPolicy.readable` as the allowlist.
pub(crate) fn sanitize_inbound_meta(
    meta: Vec<MetaEntry>,
    caps: &BlockCapabilities,
    stripped_names: &mut Vec<String>,
) -> Vec<MetaEntry> {
    meta.into_iter()
        .filter(|e| {
            let Some(name) = header_name_from_meta_key(&e.key) else {
                return true;
            };
            if !is_sensitive_header(&name, &caps.headers.masked) {
                return true;
            }
            let allowed = caps
                .headers
                .readable
                .iter()
                .any(|r| r.eq_ignore_ascii_case(&name));
            if !allowed {
                if !stripped_names.iter().any(|n| n == &name) {
                    stripped_names.push(name);
                }
                return false;
            }
            true
        })
        .collect()
}

#[cfg(test)]
mod header_name_tests {
    use super::header_name_from_meta_key;

    #[test]
    fn req_header_prefix() {
        assert_eq!(
            header_name_from_meta_key("req.header.authorization"),
            Some("authorization".to_string())
        );
    }

    #[test]
    fn req_header_uppercase_lowercased() {
        assert_eq!(
            header_name_from_meta_key("req.header.Authorization"),
            Some("authorization".to_string())
        );
    }

    #[test]
    fn resp_header_prefix() {
        assert_eq!(
            header_name_from_meta_key("resp.header.x-custom"),
            Some("x-custom".to_string())
        );
    }

    #[test]
    fn legacy_resp_set_cookie_bare() {
        assert_eq!(
            header_name_from_meta_key("resp.set_cookie"),
            Some("set-cookie".to_string())
        );
    }

    #[test]
    fn legacy_resp_set_cookie_nested() {
        assert_eq!(
            header_name_from_meta_key("resp.set_cookie.session"),
            Some("set-cookie".to_string())
        );
    }

    #[test]
    fn internal_meta_key_is_none() {
        assert_eq!(header_name_from_meta_key("auth.user_id"), None);
        assert_eq!(header_name_from_meta_key("trace_id"), None);
        assert_eq!(header_name_from_meta_key(""), None);
    }
}

#[cfg(test)]
mod sanitize_tests {
    use wafer_block::capabilities::{BlockCapabilities, HeaderPolicy};

    use super::*;

    fn meta(key: &str, value: &str) -> MetaEntry {
        MetaEntry {
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn outbound_strips_default_sensitive_when_empty_policy() {
        let caps = BlockCapabilities::default();
        let input = vec![
            meta("resp.header.content-type", "text/plain"),
            meta("resp.header.set-cookie", "s=1"),
            meta("resp.set_cookie", "legacy"),
            meta("resp.header.x-frame-options", "DENY"),
            meta("resp.header.x-safe", "ok"),
        ];
        let mut stripped = Vec::new();
        let out = sanitize_outbound_meta(input, &caps, &mut stripped);
        let keys: Vec<&str> = out.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"resp.header.content-type"));
        assert!(keys.contains(&"resp.header.x-safe"));
        assert!(!keys
            .iter()
            .any(|k| k.contains("set-cookie") || k.contains("set_cookie")));
        assert!(!keys.iter().any(|k| k.contains("x-frame-options")));
        assert!(stripped.contains(&"set-cookie".to_string()));
    }

    #[test]
    fn outbound_writable_allows_named_header() {
        let caps = BlockCapabilities {
            headers: HeaderPolicy {
                writable: vec!["set-cookie".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let input = vec![
            meta("resp.header.set-cookie", "s=1"),
            meta("resp.set_cookie", "legacy"),
            meta("resp.header.x-frame-options", "DENY"),
        ];
        let mut stripped = Vec::new();
        let out = sanitize_outbound_meta(input, &caps, &mut stripped);
        let keys: Vec<&str> = out.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"resp.header.set-cookie"));
        assert!(keys.contains(&"resp.set_cookie"));
        assert!(!keys.iter().any(|k| k.contains("x-frame-options")));
    }

    #[test]
    fn inbound_strips_default_sensitive_when_empty_policy() {
        let caps = BlockCapabilities::default();
        let input = vec![
            meta("req.header.accept", "text/plain"),
            meta("req.header.authorization", "Bearer abc"),
            meta("req.header.cookie", "a=1"),
        ];
        let mut stripped = Vec::new();
        let out = sanitize_inbound_meta(input, &caps, &mut stripped);
        let keys: Vec<&str> = out.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"req.header.accept"));
        assert!(!keys.iter().any(|k| k.contains("authorization")));
        assert!(!keys.iter().any(|k| k.contains("cookie")));
    }

    #[test]
    fn inbound_readable_allows_named_header() {
        let caps = BlockCapabilities {
            headers: HeaderPolicy {
                readable: vec!["authorization".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let input = vec![
            meta("req.header.authorization", "Bearer abc"),
            meta("req.header.cookie", "a=1"),
        ];
        let mut stripped = Vec::new();
        let out = sanitize_inbound_meta(input, &caps, &mut stripped);
        let keys: Vec<&str> = out.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"req.header.authorization"));
        assert!(!keys.iter().any(|k| k.contains("cookie")));
    }

    #[test]
    fn masked_extends_default_sensitive_both_directions() {
        let caps = BlockCapabilities {
            headers: HeaderPolicy {
                masked: vec!["x-internal".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let inbound = vec![meta("req.header.x-internal", "secret")];
        let outbound = vec![meta("resp.header.x-internal", "secret")];
        let mut s1 = Vec::new();
        let mut s2 = Vec::new();
        assert!(sanitize_inbound_meta(inbound, &caps, &mut s1).is_empty());
        assert!(sanitize_outbound_meta(outbound, &caps, &mut s2).is_empty());
    }

    #[test]
    fn non_header_keys_pass_through() {
        let caps = BlockCapabilities::default();
        let input = vec![meta("auth.user_id", "u1"), meta("trace_id", "abc")];
        let mut s = Vec::new();
        let out = sanitize_outbound_meta(input, &caps, &mut s);
        let keys: Vec<&str> = out.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"auth.user_id"));
        assert!(keys.contains(&"trace_id"));
    }
}

// ---------------------------------------------------------------------------
// Host state stored in the wasmi Store
// ---------------------------------------------------------------------------

struct WasmiHostState {
    /// Context reference — set before each guest call via ContextGuard.
    context: Option<Arc<dyn Context>>,
    /// Capabilities (resource limits) for this block.
    /// Used by host function enforcement (e.g. `allows_call_block`).
    capabilities: BlockCapabilities,
    /// Per-instance stream registry. Drops with the Store, cancelling any
    /// in-flight response streams via their paired `CancellationToken`s.
    streams: StreamRegistry,
    /// Set by __wafer_host_stream_finish to request the host resume loop
    /// drive `Context::call_block` for this handle. The loop calls
    /// `take_finish_request` on the StreamState, dispatches, and installs
    /// the resulting OutputStream on the StreamState before resuming the
    /// guest with the i32 status code (0 = ok, negative = ErrorCode).
    pending_stream_finish: Option<u64>,
    /// Set by __wafer_host_stream_read_chunk to request the host resume loop
    /// pull the next frame off the response stream. The loop allocates guest
    /// memory for the bytes (if any) and resumes with the packed (ptr, len)
    /// — or 0 for end-of-stream — or a negative ErrorCode sentinel.
    pending_stream_read: Option<u64>,
    /// Set by __wafer_host_stream_take_error to request the host resume loop
    /// allocate guest memory and write the rmp-serde-encoded WaferError. The
    /// loop resumes with packed (ptr, len), or 0 if no error is present.
    pending_stream_take_error: Option<u64>,
    /// Set by __wafer_host_load_asset to request an async asset load.
    /// The resume loop consumes this, drives the LoadAssetCallback, and
    /// resumes the guest with the resolved i32 status code as the return
    /// value (wasmi's `resumable.resume(..)` value IS the return value of
    /// the trapped host function — no phase-2 re-entry like call_block).
    pending_load_asset: Option<String>,
    /// Per-call-frame inbound attachments. Populated by the runtime before
    /// `__wafer_handle` is invoked; consulted by the
    /// `__wafer_host_lookup_attachment` host import. `None` for top-level
    /// calls (e.g. router-initiated requests) and intermediate states where
    /// the slot has not yet been seeded.
    pub(crate) current_attachments:
        Option<std::collections::BTreeMap<String, wafer_block::Attachment>>,
}

impl wasmi::ResourceLimiter for WasmiHostState {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, wasmi::errors::MemoryError> {
        // One WASM page = 64 KiB.
        let desired_pages = desired / 65536;
        Ok(desired_pages <= MAX_WASM_MEMORY_PAGES)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, wasmi::errors::TableError> {
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Sentinel errors for trap+resume
// ---------------------------------------------------------------------------

/// Marker trap for `__wafer_host_stream_finish` — the resume loop catches this
/// and dispatches the call to `Context::call_block`, installing the resulting
/// `OutputStream` on the StreamState before resuming the guest with an i32
/// status code.
#[derive(Debug)]
struct StreamFinishTrap;

impl std::fmt::Display for StreamFinishTrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stream_finish trap (expected — will be resumed)")
    }
}

impl wasmi::core::HostError for StreamFinishTrap {}

/// Marker trap for `__wafer_host_stream_read_chunk` — the resume loop drives
/// `OutputStream::next()`, allocates guest memory for the bytes (if any), and
/// resumes the guest with the packed (ptr, len) i64.
#[derive(Debug)]
struct StreamReadTrap;

impl std::fmt::Display for StreamReadTrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stream_read trap (expected — will be resumed)")
    }
}

impl wasmi::core::HostError for StreamReadTrap {}

/// Marker trap for `__wafer_host_stream_take_error` — the resume loop pops the
/// stream's `last_error`, encodes it via rmp-serde, allocates guest memory, and
/// resumes with the packed (ptr, len) i64.
#[derive(Debug)]
struct StreamTakeErrorTrap;

impl std::fmt::Display for StreamTakeErrorTrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stream_take_error trap (expected — will be resumed)")
    }
}

impl wasmi::core::HostError for StreamTakeErrorTrap {}

/// Marker error returned by `__wafer_host_load_asset` to suspend execution.
/// The resume loop catches it and drives the registered `LoadAssetCallback`
/// before resuming the guest.
#[derive(Debug)]
struct LoadAssetTrap;

impl std::fmt::Display for LoadAssetTrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "load_asset trap (expected — will be resumed)")
    }
}

impl wasmi::core::HostError for LoadAssetTrap {}

// ---------------------------------------------------------------------------
// Negative-i64 / negative-i32 ErrorCode sentinels for the streaming ABI
// ---------------------------------------------------------------------------

/// Map a `WaferError` to a negative `i32` sentinel suitable for returning from
/// host imports declared as `... -> i32`. The low byte carries an opaque
/// numeric code corresponding to the `ErrorCode` discriminant; `-1` is the
/// generic fallback. The guest unpacks via `take_error` for full details.
fn error_code_to_neg_i32(code: ErrorCode) -> i32 {
    -(error_code_ordinal(code) as i32)
}

/// Negative-i64 variant. Same encoding as `error_code_to_neg_i32` but widened.
fn error_code_to_neg_i64(code: ErrorCode) -> i64 {
    -(error_code_ordinal(code) as i64)
}

/// Stable opaque numeric tag for an `ErrorCode`. We hand-roll this rather than
/// using `as i32` on the enum so the wire mapping is independent of source
/// ordering. Values are 1..=17 (skipping 0 which means "ok / no error"); the
/// guest's `take_error` is the source of truth for full structured details.
fn error_code_ordinal(code: ErrorCode) -> u8 {
    match code {
        ErrorCode::Ok => 0,
        ErrorCode::Cancelled => 1,
        ErrorCode::Unknown => 2,
        ErrorCode::InvalidArgument => 3,
        ErrorCode::DeadlineExceeded => 4,
        ErrorCode::NotFound => 5,
        ErrorCode::AlreadyExists => 6,
        ErrorCode::PermissionDenied => 7,
        ErrorCode::ResourceExhausted => 8,
        ErrorCode::FailedPrecondition => 9,
        ErrorCode::Aborted => 10,
        ErrorCode::OutOfRange => 11,
        ErrorCode::Unimplemented => 12,
        ErrorCode::Internal => 13,
        ErrorCode::Unavailable => 14,
        ErrorCode::DataLoss => 15,
        ErrorCode::Unauthenticated => 16,
    }
}

// ---------------------------------------------------------------------------
// Guest memory helpers
// ---------------------------------------------------------------------------

/// Read `len` bytes starting at `offset` from the guest's exported `memory`.
fn read_guest_bytes(
    store: &Store<WasmiHostState>,
    memory: wasmi::Memory,
    offset: u32,
    len: u32,
) -> Result<Vec<u8>, RuntimeError> {
    let mut buf = vec![0u8; len as usize];
    memory
        .read(store, offset as usize, &mut buf)
        .map_err(|e| RuntimeError::Wasm(format!("reading guest memory at {offset}+{len}: {e}")))?;
    Ok(buf)
}

/// Allocate space in guest memory via `__wafer_alloc`, then write `data`.
/// Returns the guest pointer.
fn write_guest_bytes(
    store: &mut Store<WasmiHostState>,
    alloc_fn: wasmi::TypedFunc<i32, i32>,
    memory: wasmi::Memory,
    data: &[u8],
) -> Result<u32, RuntimeError> {
    let len = data.len() as i32;
    let ptr = alloc_fn
        .call(&mut *store, len)
        .map_err(|e| RuntimeError::Wasm(format!("__wafer_alloc({len}): {e}")))?;
    memory
        .write(&mut *store, ptr as usize, data)
        .map_err(|e| RuntimeError::Wasm(format!("writing {len} bytes at guest ptr {ptr}: {e}")))?;
    Ok(ptr as u32)
}

// ---------------------------------------------------------------------------
// Linker setup
// ---------------------------------------------------------------------------

fn build_linker(engine: &Engine) -> Result<Linker<WasmiHostState>, RuntimeError> {
    let mut linker = Linker::<WasmiHostState>::new(engine);

    // ---- wafer module: host imports ----

    // __wafer_host_is_cancelled() -> i32
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_is_cancelled",
            |caller: Caller<WasmiHostState>| -> i32 {
                let state = caller.data();
                if let Some(ref ctx) = state.context {
                    if ctx.is_cancelled() {
                        1
                    } else {
                        0
                    }
                } else {
                    0
                }
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_is_cancelled: {e}")))?;

    // __wafer_host_log(level_ptr, level_len, msg_ptr, msg_len)
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_log",
            |caller: Caller<WasmiHostState>,
             level_ptr: i32,
             level_len: i32,
             msg_ptr: i32,
             msg_len: i32| {
                let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return;
                };
                let level_bytes = {
                    let mut buf = vec![0u8; level_len as usize];
                    if memory.read(&caller, level_ptr as usize, &mut buf).is_err() {
                        return;
                    }
                    buf
                };
                let msg_bytes = {
                    let mut buf = vec![0u8; msg_len as usize];
                    if memory.read(&caller, msg_ptr as usize, &mut buf).is_err() {
                        return;
                    }
                    buf
                };
                let level = String::from_utf8_lossy(&level_bytes);
                let msg = String::from_utf8_lossy(&msg_bytes);
                match level.as_ref() {
                    "error" => tracing::error!(target: "wasm_guest", "{msg}"),
                    "warn" => tracing::warn!(target: "wasm_guest", "{msg}"),
                    "debug" => tracing::debug!(target: "wasm_guest", "{msg}"),
                    "trace" => tracing::trace!(target: "wasm_guest", "{msg}"),
                    _ => tracing::info!(target: "wasm_guest", "{msg}"),
                }
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_log: {e}")))?;

    // ---- Streaming call ABI: 6 host imports ------------------------------
    //
    // Lifecycle: stream_init → write_chunk* → finish → read_chunk* → close.
    // See `wasm/stream.rs` for the StreamState state machine.
    //
    // Synchronous host imports (no trap): init, write_chunk, close.
    // Trap+resume host imports (need async dispatch or guest allocation):
    // finish, read_chunk, take_error.

    // __wafer_host_stream_init(name_ptr, name_len, msg_ptr, msg_len) -> i64
    //
    // Reads the target block name + serialized Message from guest memory,
    // allocates a fresh StreamState in the registry, returns the handle as
    // a positive i64. On capability denial returns a negative i64 carrying
    // the ErrorCode sentinel (the guest can call `take_error` for details
    // — but `init` failure leaves no handle, so the SDK must treat negative
    // returns as immediate errors without further state.)
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_init",
            |mut caller: Caller<WasmiHostState>,
             name_ptr: i32,
             name_len: i32,
             msg_ptr: i32,
             msg_len: i32|
             -> Result<i64, WasmiError> {
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| e.into_memory())
                    .ok_or_else(|| WasmiError::new("guest has no exported memory"))?;

                let mut name_buf = vec![0u8; name_len as usize];
                memory
                    .read(&caller, name_ptr as usize, &mut name_buf)
                    .map_err(|e| WasmiError::new(format!("reading block name: {e}")))?;
                let block_name = String::from_utf8(name_buf)
                    .map_err(|e| WasmiError::new(format!("block name not UTF-8: {e}")))?;

                // Capability check: deny if block is not allowed to call target.
                if !caller.data().capabilities.allows_call_block(&block_name) {
                    return Ok(error_code_to_neg_i64(ErrorCode::PermissionDenied));
                }

                let mut msg_buf = vec![0u8; msg_len as usize];
                memory
                    .read(&caller, msg_ptr as usize, &mut msg_buf)
                    .map_err(|e| WasmiError::new(format!("reading stream message: {e}")))?;

                // Decode the message. The SDK encodes via rmp-serde (codec
                // module); historically the guest sent JSON. We try rmp first
                // and fall back to JSON for forward compatibility with any
                // pre-codec callers — but the production path is rmp.
                let msg: Message = match wafer_block::codec::decode::<Message>(&msg_buf) {
                    Ok(m) => m,
                    Err(_) => match serde_json::from_slice::<Message>(&msg_buf) {
                        Ok(m) => m,
                        Err(_) => {
                            return Ok(error_code_to_neg_i64(ErrorCode::InvalidArgument));
                        }
                    },
                };

                let state = StreamState::new(block_name, msg);
                let handle = caller.data_mut().streams.alloc(state);
                Ok(handle as i64)
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_stream_init: {e}")))?;

    // __wafer_host_stream_write_chunk(handle, body_ptr, body_len) -> i32
    //
    // Append a chunk to the request buffer for the given handle. Returns 0
    // on success, negative ErrorCode sentinel on error.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_write_chunk",
            |caller: Caller<WasmiHostState>,
             handle: i64,
             body_ptr: i32,
             body_len: i32|
             -> Result<i32, WasmiError> {
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| e.into_memory())
                    .ok_or_else(|| WasmiError::new("guest has no exported memory"))?;

                let mut buf = vec![0u8; body_len as usize];
                if body_len > 0 {
                    memory
                        .read(&caller, body_ptr as usize, &mut buf)
                        .map_err(|e| WasmiError::new(format!("reading stream chunk: {e}")))?;
                }

                let mut caller = caller;
                let Some(state) = caller.data_mut().streams.get_mut(handle as u64) else {
                    return Ok(error_code_to_neg_i32(ErrorCode::NotFound));
                };
                match state.write_chunk(&buf) {
                    Ok(()) => Ok(0),
                    Err(e) => {
                        let code = e.code;
                        state.record_error_and_close(e);
                        Ok(error_code_to_neg_i32(code))
                    }
                }
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_stream_write_chunk: {e}")))?;

    // __wafer_host_stream_attach(handle, payload_ptr, payload_len) -> i32
    //
    // Decodes a rmp-encoded (id: String, attachment: Attachment) tuple from
    // guest memory and adds it to the caller StreamState's attachments map.
    // Returns 0 on success, negative ErrorCode sentinel on error:
    //   - NotFound: stream handle invalid
    //   - FailedPrecondition: stream not in WritingRequest phase
    //   - InvalidArgument: payload undecodable
    //   - Internal: unrecoverable host-side error
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_attach",
            |mut caller: Caller<WasmiHostState>,
             handle: i64,
             payload_ptr: i32,
             payload_len: i32|
             -> Result<i32, WasmiError> {
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| e.into_memory())
                    .ok_or_else(|| WasmiError::new("guest has no exported memory".to_string()))?;
                let mut buf = vec![0u8; payload_len as usize];
                memory
                    .read(&caller, payload_ptr as usize, &mut buf)
                    .map_err(|e| WasmiError::new(format!("reading attach payload: {e}")))?;

                let (id, att): (String, wafer_block::Attachment) =
                    match wafer_block::codec::decode(&buf) {
                        Ok(v) => v,
                        Err(_) => return Ok(error_code_to_neg_i32(ErrorCode::InvalidArgument)),
                    };

                let Some(stream_state) = caller.data_mut().streams.get_mut(handle as u64) else {
                    return Ok(error_code_to_neg_i32(ErrorCode::NotFound));
                };

                match stream_state.attach(id, att) {
                    Ok(()) => Ok(0),
                    Err(_) => Ok(error_code_to_neg_i32(ErrorCode::FailedPrecondition)),
                }
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_stream_attach: {e}")))?;

    // __wafer_host_stream_finish(handle) -> i32
    //
    // Traps. The resume loop dispatches `Context::call_block(target, msg,
    // InputStream::from_bytes(buf))`, installs the OutputStream on the
    // StreamState, and resumes with 0. On dispatch error the loop records
    // the error on the state and resumes with a negative ErrorCode sentinel.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_finish",
            |mut caller: Caller<WasmiHostState>, handle: i64| -> Result<i32, WasmiError> {
                if caller.data_mut().streams.get_mut(handle as u64).is_none() {
                    return Ok(error_code_to_neg_i32(ErrorCode::NotFound));
                }
                caller.data_mut().pending_stream_finish = Some(handle as u64);
                Err(WasmiError::host(StreamFinishTrap))
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_stream_finish: {e}")))?;

    // __wafer_host_stream_read_chunk(handle) -> i64
    //
    // Traps. The resume loop drives the OutputStream's next frame; on a
    // Chunk it allocates guest memory + writes the bytes and resumes with
    // the packed (ptr, len). On Complete it resumes with 0 (end-of-stream).
    // On Error/Drop/Continue it records the error on the state and resumes
    // with a negative ErrorCode sentinel.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_read_chunk",
            |mut caller: Caller<WasmiHostState>, handle: i64| -> Result<i64, WasmiError> {
                if caller.data_mut().streams.get_mut(handle as u64).is_none() {
                    return Ok(error_code_to_neg_i64(ErrorCode::NotFound));
                }
                caller.data_mut().pending_stream_read = Some(handle as u64);
                Err(WasmiError::host(StreamReadTrap))
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_stream_read_chunk: {e}")))?;

    // __wafer_host_stream_take_error(handle) -> i64
    //
    // Traps. The resume loop pops `last_error` off the StreamState, encodes
    // via rmp-serde, allocates guest memory + writes, resumes with packed
    // (ptr, len). If no error is present, resumes with 0.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_take_error",
            |mut caller: Caller<WasmiHostState>, handle: i64| -> Result<i64, WasmiError> {
                if caller.data_mut().streams.get_mut(handle as u64).is_none() {
                    return Ok(error_code_to_neg_i64(ErrorCode::NotFound));
                }
                caller.data_mut().pending_stream_take_error = Some(handle as u64);
                Err(WasmiError::host(StreamTakeErrorTrap))
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_stream_take_error: {e}")))?;

    // __wafer_host_stream_close(handle)
    //
    // Synchronous: drop the stream. Idempotent — no-op if the handle is
    // unknown. Cancels any in-flight response stream via the OutputStream
    // drop on its CancellationToken.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_stream_close",
            |mut caller: Caller<WasmiHostState>, handle: i64| {
                caller.data_mut().streams.close(handle as u64);
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_stream_close: {e}")))?;

    // __wafer_host_lookup_attachment(id_ptr, id_len) -> i64
    //
    // Returns:
    //   - Negative ErrorCode sentinel (NotFound) if the current call frame has
    //     no attachment under id.
    //   - Negative ErrorCode sentinel (InvalidArgument) if id is not valid UTF-8.
    //   - Negative ErrorCode sentinel (Internal) if encoding fails or guest-memory
    //     allocation/write fails.
    //   - Otherwise, positive packed (ptr, len) of an rmp-encoded Attachment,
    //     written via the guest's __wafer_alloc export. Guest owns the allocation.
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_lookup_attachment",
            |mut caller: Caller<WasmiHostState>,
             id_ptr: i32,
             id_len: i32|
             -> Result<i64, WasmiError> {
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| e.into_memory())
                    .ok_or_else(|| WasmiError::new("guest has no exported memory".to_string()))?;

                let mut id_buf = vec![0u8; id_len as usize];
                memory
                    .read(&caller, id_ptr as usize, &mut id_buf)
                    .map_err(|e| WasmiError::new(format!("reading attachment id: {e}")))?;

                let id = match std::str::from_utf8(&id_buf) {
                    Ok(s) => s.to_string(),
                    Err(_) => return Ok(error_code_to_neg_i64(ErrorCode::InvalidArgument)),
                };

                // Clone the attachment before any mutable borrow of caller.
                let att = match caller
                    .data()
                    .current_attachments
                    .as_ref()
                    .and_then(|m| m.get(&id))
                {
                    Some(a) => a.clone(),
                    None => return Ok(error_code_to_neg_i64(ErrorCode::NotFound)),
                };

                // Encode the Attachment via rmp.
                let Ok(encoded) = wafer_block::codec::encode(&att) else {
                    return Ok(error_code_to_neg_i64(ErrorCode::Internal));
                };

                // Allocate guest memory via __wafer_alloc (same pattern used in
                // write_guest_bytes), then write the encoded bytes. The guest
                // owns the allocation after this call returns.
                let alloc_func = caller
                    .get_export("__wafer_alloc")
                    .and_then(|e| e.into_func())
                    .ok_or_else(|| {
                        WasmiError::new("guest has no __wafer_alloc export".to_string())
                    })?;

                let alloc_fn = alloc_func
                    .typed::<i32, i32>(&caller)
                    .map_err(|e| WasmiError::new(format!("typing __wafer_alloc: {e}")))?;

                let ptr = alloc_fn
                    .call(&mut caller, encoded.len() as i32)
                    .map_err(|e| {
                        WasmiError::new(format!("__wafer_alloc({}): {e}", encoded.len()))
                    })?;

                memory
                    .write(&mut caller, ptr as usize, &encoded)
                    .map_err(|_| {
                        WasmiError::new("writing attachment bytes to guest memory".to_string())
                    })?;

                Ok(pack_ptr_len(ptr as u32, encoded.len() as u32))
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_lookup_attachment: {e}")))?;

    // __wafer_host_load_asset(id_ptr, id_len) -> i32
    //
    // Reads the asset id from guest memory, stashes it in pending_load_asset,
    // and traps. The resume loop in `call_guest_resumable` drives the
    // registered `LoadAssetCallback` and resumes the guest with the resolved
    // i32 status code as the return value. (Unlike `__wafer_host_call_block`,
    // this host function has no phase-2 re-entry — the resume value IS the
    // return value in wasmi's resumable-call API.)
    //
    // Status codes:
    //   0 = Ready, 1 = Pending, 2 = Failed
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_load_asset",
            |mut caller: Caller<WasmiHostState>,
             id_ptr: i32,
             id_len: i32|
             -> Result<i32, WasmiError> {
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| e.into_memory())
                    .ok_or_else(|| WasmiError::new("guest has no exported memory"))?;

                let mut id_buf = vec![0u8; id_len as usize];
                memory
                    .read(&caller, id_ptr as usize, &mut id_buf)
                    .map_err(|e| WasmiError::new(format!("reading asset id: {e}")))?;
                let asset_id = String::from_utf8(id_buf)
                    .map_err(|e| WasmiError::new(format!("asset id not UTF-8: {e}")))?;

                caller.data_mut().pending_load_asset = Some(asset_id);
                Err(WasmiError::host(LoadAssetTrap))
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_load_asset: {e}")))?;

    // ---- WASI stubs (wasi_snapshot_preview1) ----

    // fd_write(fd, iovs_ptr, iovs_len, nwritten_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "fd_write",
            |mut caller: Caller<WasmiHostState>,
             _fd: i32,
             _iovs_ptr: i32,
             _iovs_len: i32,
             nwritten_ptr: i32|
             -> i32 {
                // Discard output. Write 0 to nwritten.
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let _ = memory.write(&mut caller, nwritten_ptr as usize, &0u32.to_le_bytes());
                }
                0 // __WASI_ERRNO_SUCCESS
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking fd_write stub: {e}")))?;

    // proc_exit(code)
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "proc_exit",
            |_caller: Caller<WasmiHostState>, code: i32| -> Result<(), WasmiError> {
                Err(WasmiError::new(format!("guest called proc_exit({code})")))
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking proc_exit stub: {e}")))?;

    // environ_sizes_get(argc_ptr, argv_buf_size_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "environ_sizes_get",
            |mut caller: Caller<WasmiHostState>, argc_ptr: i32, argv_buf_size_ptr: i32| -> i32 {
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let _ = memory.write(&mut caller, argc_ptr as usize, &0u32.to_le_bytes());
                    let _ =
                        memory.write(&mut caller, argv_buf_size_ptr as usize, &0u32.to_le_bytes());
                }
                0
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking environ_sizes_get stub: {e}")))?;

    // environ_get(argv_ptr, argv_buf_ptr) -> errno
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "environ_get",
            |_caller: Caller<WasmiHostState>, _argv_ptr: i32, _argv_buf_ptr: i32| -> i32 { 0 },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking environ_get stub: {e}")))?;

    // args_sizes_get(argc_ptr, argv_buf_size_ptr) -> errno
    // TinyGo WASM runtime imports this to enumerate command-line arguments.
    // We expose zero arguments.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "args_sizes_get",
            |mut caller: Caller<WasmiHostState>, argc_ptr: i32, argv_buf_size_ptr: i32| -> i32 {
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let _ = memory.write(&mut caller, argc_ptr as usize, &0u32.to_le_bytes());
                    let _ =
                        memory.write(&mut caller, argv_buf_size_ptr as usize, &0u32.to_le_bytes());
                }
                0
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking args_sizes_get stub: {e}")))?;

    // args_get(argv_ptr, argv_buf_ptr) -> errno
    // TinyGo WASM runtime imports this to read command-line arguments.
    // We expose zero arguments so this is a no-op.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "args_get",
            |_caller: Caller<WasmiHostState>, _argv_ptr: i32, _argv_buf_ptr: i32| -> i32 { 0 },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking args_get stub: {e}")))?;

    // clock_time_get(id, precision, time_ptr) -> errno
    // WASI clock IDs (we only differentiate REALTIME vs. anything-else):
    //   0 = CLOCK_REALTIME      → wall clock, ns since UNIX epoch
    //   1 = CLOCK_MONOTONIC     → ns since an arbitrary fixed point
    //   2 = CLOCK_PROCESS_CPUTIME_ID
    //   3 = CLOCK_THREAD_CPUTIME_ID
    // We use `web_time` so this works both natively and when the runtime is
    // itself compiled to WASM and embedded in a browser (solobase-web): on
    // wasm32 the call delegates to `Date.now()` / `Performance.now()`.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "clock_time_get",
            |mut caller: Caller<WasmiHostState>, id: i32, _precision: i64, time_ptr: i32| -> i32 {
                let nanos: u64 = if id == 0 {
                    web_time::SystemTime::now()
                        .duration_since(web_time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0)
                } else {
                    // For monotonic/CPU-time IDs, fall back to an Instant-based
                    // counter relative to the runtime's own start. This is the
                    // best we can do without process boot-time, and it is
                    // monotonic — sufficient for chrono's elapsed-time paths.
                    static START: std::sync::OnceLock<web_time::Instant> =
                        std::sync::OnceLock::new();
                    let start = START.get_or_init(web_time::Instant::now);
                    web_time::Instant::now().duration_since(*start).as_nanos() as u64
                };
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let _ = memory.write(&mut caller, time_ptr as usize, &nanos.to_le_bytes());
                }
                0
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking clock_time_get stub: {e}")))?;

    // random_get(buf_ptr, buf_len) -> errno
    // TinyGo WASM runtime imports this for crypto/rand and map seed initialisation.
    // We fill the buffer with real random bytes via getrandom.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "random_get",
            |mut caller: Caller<WasmiHostState>, buf_ptr: i32, buf_len: i32| -> i32 {
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let mut buf = vec![0u8; buf_len as usize];
                    if getrandom::getrandom(&mut buf).is_err() {
                        // RNG failure should not happen; fall back to zeros.
                        warn!("getrandom failed in WASI random_get, falling back to zeros");
                    }
                    let _ = memory.write(&mut caller, buf_ptr as usize, &buf);
                }
                0
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking random_get stub: {e}")))?;

    // ---- Spike-only host imports (gated behind the `spike` feature) ----
    //
    // These imports exist solely to measure per-chunk overhead of the
    // proposed streaming ABI design. They are NOT part of the production
    // ABI and are removed when the real `__wafer_host_stream_*` imports
    // land in Task 7.
    #[cfg(feature = "spike")]
    register_spike_imports(&mut linker)?;

    Ok(linker)
}

// ---------------------------------------------------------------------------
// Spike host imports (gate validation only)
// ---------------------------------------------------------------------------

/// Throwaway host imports used by `tests/streaming_spike.rs` to measure
/// per-chunk overhead of N back-to-back host→guest data transfers.
///
/// The host writes a recognisable byte pattern (`b[i] = i % 256`) into a
/// guest-provided buffer. The guest verifies the pattern and accumulates the
/// total bytes read. When the host's per-thread call counter reaches the
/// configured target, `next_chunk` returns 0 (end-of-stream) and resets state.
///
/// This deliberately avoids re-entering `__wafer_alloc` from inside a host
/// call — the streaming ABI design assumes the host writes into a buffer the
/// guest has pre-allocated. Recursive guest calls inside a wasmi host
/// function would also be a poor stand-in for the real ABI's straight-line
/// per-chunk cost.
#[cfg(feature = "spike")]
fn register_spike_imports(linker: &mut Linker<WasmiHostState>) -> Result<(), RuntimeError> {
    use std::cell::RefCell;

    thread_local! {
        /// (current chunk count, target chunk count). Reset to (0, target)
        /// by `set_target` and incremented by each `next_chunk` call. When
        /// `current == target` the next `next_chunk` returns 0 and resets.
        static SPIKE_STATE: RefCell<(u32, u32)> = const { RefCell::new((0, 100)) };
    }

    // wafer_spike::set_target(n: i32)
    //
    // Resets the per-thread chunk counter and sets the target number of
    // chunks before `next_chunk` returns end-of-stream.
    linker
        .func_wrap(
            "wafer_spike",
            "set_target",
            |_caller: Caller<WasmiHostState>, n: i32| {
                SPIKE_STATE.with(|s| {
                    let mut s = s.borrow_mut();
                    *s = (0, n.max(0) as u32);
                });
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking wafer_spike::set_target: {e}")))?;

    // wafer_spike::next_chunk(buf_ptr: i32, chunk_size: i32) -> i32
    //
    // If `current < target`: writes `chunk_size` bytes of the pattern
    // `b[i] = i % 256` to guest memory at `buf_ptr`, increments the counter,
    // returns 1.
    // Otherwise: resets the counter to 0 (target preserved) and returns 0.
    linker
        .func_wrap(
            "wafer_spike",
            "next_chunk",
            |mut caller: Caller<WasmiHostState>, buf_ptr: i32, chunk_size: i32| -> i32 {
                let done = SPIKE_STATE.with(|s| {
                    let mut st = s.borrow_mut();
                    if st.0 >= st.1 {
                        st.0 = 0;
                        true
                    } else {
                        st.0 += 1;
                        false
                    }
                });
                if done {
                    return 0;
                }
                let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return 0;
                };
                // Build the recognisable pattern. For 512 KiB this allocates
                // a 512 KiB Vec per call — that's exactly the cost we want
                // to measure (host-side encode + guest write).
                let n = chunk_size.max(0) as usize;
                let mut buf = vec![0u8; n];
                for (i, b) in buf.iter_mut().enumerate() {
                    *b = (i % 256) as u8;
                }
                if memory.write(&mut caller, buf_ptr as usize, &buf).is_err() {
                    return 0;
                }
                1
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking wafer_spike::next_chunk: {e}")))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Spike test entry point
// ---------------------------------------------------------------------------

/// Spike-only helper exposed to `tests/streaming_spike.rs`.
///
/// Compiles the supplied `wasm_bytes` (the spike guest), instantiates it
/// with the `wafer_spike::*` host imports wired up, calls
/// `spike_set_target(target_chunks)` followed by `read_all_chunks(chunk_size)`,
/// and returns `(total_bytes_read, elapsed_during_read)`.
///
/// The elapsed measurement isolates the streaming-loop cost — module
/// compilation, instantiation and target-config calls are NOT included.
#[cfg(feature = "spike")]
pub fn run_spike(
    wasm_bytes: &[u8],
    target_chunks: i32,
    chunk_size: i32,
) -> Result<(i64, std::time::Duration), RuntimeError> {
    let mut config = wasmi::Config::default();
    // No fuel metering for the spike — we want raw overhead numbers.
    config.consume_fuel(false);
    let engine = Engine::new(&config);

    let module = Module::new(&engine, wasm_bytes)
        .map_err(|e| RuntimeError::Wasm(format!("compiling spike guest: {e}")))?;

    let mut linker = Linker::<WasmiHostState>::new(&engine);
    register_spike_imports(&mut linker)?;

    let host_state = WasmiHostState {
        context: None,
        capabilities: BlockCapabilities::unrestricted(),
        streams: StreamRegistry::new(),
        pending_stream_finish: None,
        pending_stream_read: None,
        pending_stream_take_error: None,
        pending_load_asset: None,
        current_attachments: None,
    };
    let mut store = Store::new(&engine, host_state);

    let pre = linker
        .instantiate(&mut store, &module)
        .map_err(|e| RuntimeError::Wasm(format!("instantiating spike guest: {e}")))?;
    let instance = pre
        .start(&mut store)
        .map_err(|e| RuntimeError::Wasm(format!("starting spike guest: {e}")))?;

    let set_target_fn = instance
        .get_typed_func::<i32, ()>(&store, "spike_set_target")
        .map_err(|e| RuntimeError::Wasm(format!("getting spike_set_target: {e}")))?;
    let read_all_fn = instance
        .get_typed_func::<i32, i64>(&store, "read_all_chunks")
        .map_err(|e| RuntimeError::Wasm(format!("getting read_all_chunks: {e}")))?;

    set_target_fn
        .call(&mut store, target_chunks)
        .map_err(|e| RuntimeError::Wasm(format!("calling spike_set_target: {e}")))?;

    let start = std::time::Instant::now();
    let total = read_all_fn
        .call(&mut store, chunk_size)
        .map_err(|e| RuntimeError::Wasm(format!("calling read_all_chunks: {e}")))?;
    let elapsed = start.elapsed();

    Ok((total, elapsed))
}

// ---------------------------------------------------------------------------
// Instantiation helper
// ---------------------------------------------------------------------------

/// Create a fresh store + instance from the pre-built linker and module.
///
/// For TinyGo WASM modules (wasi target) the exported `_start` function must
/// be called after instantiation to initialise the Go runtime (allocator,
/// goroutine scheduler, global vars) and to invoke `main()` (which calls
/// `wafer.Register`). Without it every WAFER export traps with `unreachable`.
///
/// `_start` terminates by calling `proc_exit(0)` — that traps with our stub.
/// We treat a trap message containing "proc_exit" as expected WASI shutdown.
/// Rust-compiled blocks have no `_start` export and are unaffected.
fn instantiate(
    engine: &Engine,
    linker: &Linker<WasmiHostState>,
    module: &Module,
    caps: &BlockCapabilities,
) -> Result<(Store<WasmiHostState>, wasmi::Instance), RuntimeError> {
    let host_state = WasmiHostState {
        context: None,
        capabilities: caps.clone(),
        streams: StreamRegistry::new(),
        pending_stream_finish: None,
        pending_stream_read: None,
        pending_stream_take_error: None,
        pending_load_asset: None,
        current_attachments: None,
    };
    let mut store = Store::new(engine, host_state);

    // Resource limits.
    store.limiter(|state| state);
    store
        .set_fuel(DEFAULT_FUEL)
        .map_err(|e| RuntimeError::Wasm(format!("setting fuel: {e}")))?;

    let pre = linker
        .instantiate(&mut store, module)
        .map_err(|e| RuntimeError::Wasm(format!("instantiation: {e}")))?;
    let instance = pre
        .start(&mut store)
        .map_err(|e| RuntimeError::Wasm(format!("running start function: {e}")))?;

    // Call `_start` if exported — required for TinyGo WASM modules.
    if let Ok(start_fn) = instance.get_typed_func::<(), ()>(&store, "_start") {
        match start_fn.call(&mut store, ()) {
            Ok(()) => {}
            Err(e) => {
                let msg = e.to_string();
                if !msg.contains("proc_exit") {
                    return Err(RuntimeError::Wasm(format!("WASM _start failed: {e}")));
                }
                // proc_exit(0) is the normal WASI shutdown path — expected.
            }
        }
        // Re-fill fuel so the subsequent guest call has a full budget.
        store
            .set_fuel(DEFAULT_FUEL)
            .map_err(|e| RuntimeError::Wasm(format!("refilling fuel after _start: {e}")))?;
    }

    Ok((store, instance))
}

// ---------------------------------------------------------------------------
// WasmiBlock
// ---------------------------------------------------------------------------

/// `Block` implementation that runs a WASM module via the `wasmi` interpreter.
pub struct WasmiBlock {
    engine: Engine,
    module: Module,
    linker: Linker<WasmiHostState>,
    info_cache: Mutex<Option<BlockInfo>>,
    /// Interior-mutable capabilities field so the runtime can propagate the
    /// effective set (`declared ∩ config`) after `resolve()` without reloading
    /// the WASM module.  Reads are lock-free on uncontended RwLock; the write
    /// path is exercised at most once per block lifetime (startup).
    capabilities: parking_lot::RwLock<BlockCapabilities>,
    /// Warn-once flag for outbound stripped headers.
    warned_outbound: std::sync::atomic::AtomicBool,
    /// Warn-once flag for inbound stripped headers.
    warned_inbound: std::sync::atomic::AtomicBool,
    /// Host-side asset loader for external WASM/JS assets referenced by the
    /// block's `external_assets` manifest field. Defaults to `NoopAssetLoader`.
    /// Hosts inject a real loader via `set_asset_loader`.
    asset_loader: parking_lot::RwLock<Arc<dyn crate::asset_loader::LoadAssetCallback>>,
}

// Safety: Engine, Module, Linker are Send+Sync in wasmi 0.44.
// The Mutex guards the cache.
unsafe impl Send for WasmiBlock {}
unsafe impl Sync for WasmiBlock {}

impl WasmiBlock {
    /// Read a WASM module from disk and compile it (native-only convenience wrapper).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load(path: &str) -> Result<Self, RuntimeError> {
        let bytes = std::fs::read(path)
            .map_err(|e| RuntimeError::Wasm(format!("reading WASM file: {e}")))?;
        Self::load_from_bytes(&bytes)
    }

    /// Compile a WASM module from raw bytes with unrestricted host capabilities.
    pub fn load_from_bytes(wasm_bytes: &[u8]) -> Result<Self, RuntimeError> {
        Self::load_with_capabilities(wasm_bytes, BlockCapabilities::unrestricted())
    }

    /// Compile a WASM module with a custom capability set (filters host imports).
    pub fn load_with_capabilities(
        wasm_bytes: &[u8],
        caps: BlockCapabilities,
    ) -> Result<Self, RuntimeError> {
        let mut config = wasmi::Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        Self::load_with_engine(&engine, wasm_bytes, caps)
    }

    /// Compile a WASM module reusing an existing `wasmi::Engine` (lets callers share fuel config).
    pub fn load_with_engine(
        engine: &Engine,
        wasm_bytes: &[u8],
        caps: BlockCapabilities,
    ) -> Result<Self, RuntimeError> {
        let module = Module::new(engine, wasm_bytes)
            .map_err(|e| RuntimeError::Wasm(format!("compiling WASM module: {e}")))?;
        let linker = build_linker(engine)?;
        Ok(Self {
            engine: engine.clone(),
            module,
            linker,
            info_cache: Mutex::new(None),
            capabilities: parking_lot::RwLock::new(caps),
            warned_outbound: std::sync::atomic::AtomicBool::new(false),
            warned_inbound: std::sync::atomic::AtomicBool::new(false),
            asset_loader: parking_lot::RwLock::new(Arc::new(crate::asset_loader::NoopAssetLoader)),
        })
    }

    /// Replace the asset loader used by `__wafer_host_load_asset`. Called by
    /// hosts (e.g. solobase-web) during startup to inject a real loader that
    /// fetches CDN assets, verifies sha256, and returns readiness.
    pub fn set_asset_loader(&self, loader: Arc<dyn crate::asset_loader::LoadAssetCallback>) {
        *self.asset_loader.write() = loader;
    }

    /// Return the currently active asset loader. Used by tests to verify that
    /// propagation from `Wafer::set_asset_loader` / `Wafer::register_block`
    /// has taken effect.
    #[cfg(test)]
    pub fn asset_loader_for_test(&self) -> Arc<dyn crate::asset_loader::LoadAssetCallback> {
        self.asset_loader.read().clone()
    }

    /// Variant of `Block::handle` that seeds inbound attachments visible to
    /// the guest via `__wafer_host_lookup_attachment`. Called by
    /// `RuntimeContext::call_block_with_attachments` when the callee is a
    /// wasmi block.
    pub(crate) async fn handle_with_attachments(
        &self,
        ctx: &dyn Context,
        msg: Message,
        input: InputStream,
        attachments: std::collections::BTreeMap<String, wafer_block::Attachment>,
    ) -> OutputStream {
        self.handle_inner(ctx, msg, input, Some(attachments)).await
    }

    /// Shared body of `handle` / `handle_with_attachments`. Serialises
    /// (msg, body) for the guest, drives `__wafer_handle` through the resume
    /// loop with the given attachments slot, and decodes the guest ABI result
    /// back into an `OutputStream`.
    async fn handle_inner(
        &self,
        ctx: &dyn Context,
        msg: Message,
        input: InputStream,
        attachments: Option<std::collections::BTreeMap<String, wafer_block::Attachment>>,
    ) -> OutputStream {
        let body = input.collect_to_bytes().await;

        // Sanitize inbound message meta before passing to WASM guest.
        let msg = {
            let mut stripped_in: Vec<String> = Vec::new();
            let caps_guard = self.capabilities.read();
            let sanitized_meta = sanitize_inbound_meta(msg.meta, &caps_guard, &mut stripped_in);
            drop(caps_guard);
            if !stripped_in.is_empty() {
                self.warn_once_stripped_inbound(&stripped_in);
            }
            Message {
                meta: sanitized_meta,
                ..msg
            }
        };

        let msg_bytes = match serde_json::to_vec(&(&msg, &body)) {
            Ok(b) => b,
            Err(e) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::Internal,
                    format!("serializing message: {e}"),
                ));
            }
        };

        let result_bytes = match self
            .call_guest_resumable_with_attachments(ctx, attachments, |store, instance| {
                let alloc_fn = instance
                    .get_typed_func::<i32, i32>(&*store, "__wafer_alloc")
                    .map_err(|e| RuntimeError::Wasm(format!("getting __wafer_alloc: {e}")))?;
                let handle_fn = instance
                    .get_typed_func::<(i32, i32), i64>(&*store, "__wafer_handle")
                    .map_err(|e| RuntimeError::Wasm(format!("getting __wafer_handle: {e}")))?;
                let memory = instance.get_memory(&*store, "memory").ok_or_else(|| {
                    RuntimeError::Wasm("guest has no exported memory".to_string())
                })?;

                let ptr = write_guest_bytes(store, alloc_fn, memory, &msg_bytes)?;
                let len = msg_bytes.len() as i32;
                Ok((handle_fn, ptr as i32, len))
            })
            .await
        {
            Ok(bytes) => bytes,
            Err(e) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::Internal,
                    format!("WASM handle error: {e}"),
                ));
            }
        };

        // The guest returns a guest ABI format JSON. Map it back to OutputStream.
        #[derive(serde::Deserialize)]
        struct GuestAbiResult {
            action: String,
            response: Option<GuestAbiResponse>,
            error: Option<WaferError>,
            message: Option<Message>,
        }
        #[derive(serde::Deserialize)]
        struct GuestAbiResponse {
            data: Vec<u8>,
            #[serde(default)]
            meta: Vec<MetaEntry>,
        }

        match serde_json::from_slice::<GuestAbiResult>(&result_bytes) {
            Ok(result) => match result.action.as_str() {
                "Respond" => {
                    let (data, meta) = result
                        .response
                        .map(|r| {
                            let mut stripped: Vec<String> = Vec::new();
                            let caps_guard = self.capabilities.read();
                            let sanitized =
                                sanitize_outbound_meta(r.meta, &caps_guard, &mut stripped);
                            drop(caps_guard);
                            if !stripped.is_empty() {
                                self.warn_once_stripped_outbound(&stripped);
                            }
                            (r.data, sanitized)
                        })
                        .unwrap_or_default();
                    if meta.is_empty() {
                        OutputStream::respond(data)
                    } else {
                        OutputStream::respond_with_meta(data, meta)
                    }
                }
                "Error" => {
                    let e = result.error.unwrap_or_else(|| {
                        WaferError::new(
                            ErrorCode::Internal,
                            "WASM block returned error with no details",
                        )
                    });
                    OutputStream::error(e)
                }
                "Drop" => OutputStream::drop_request(),
                "Continue" => {
                    let msg = result.message.unwrap_or_else(|| Message::new("continue"));
                    OutputStream::continue_with(msg)
                }
                _ => OutputStream::error(WaferError::new(
                    ErrorCode::Internal,
                    format!("unknown action from WASM guest: {}", result.action),
                )),
            },
            Err(e) => OutputStream::error(WaferError::new(
                ErrorCode::Internal,
                format!("deserializing WASM handle result: {e}"),
            )),
        }
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Call a guest function that returns a packed i64 (ptr << 32 | len),
    /// handling the call_block trap+resume loop.
    ///
    /// `setup` prepares the store/instance (writes args, returns the TypedFunc).
    /// When a `call_block` trap occurs the loop resolves it via `ctx` and resumes.
    async fn call_guest_resumable(
        &self,
        ctx: &dyn Context,
        setup: impl FnOnce(
            &mut Store<WasmiHostState>,
            wasmi::Instance,
        )
            -> Result<(wasmi::TypedFunc<(i32, i32), i64>, i32, i32), RuntimeError>,
    ) -> Result<Vec<u8>, RuntimeError> {
        self.call_guest_resumable_with_attachments(ctx, None, setup)
            .await
    }

    /// Variant of `call_guest_resumable` that seeds the wasmi store's
    /// `current_attachments` slot before the guest call begins. Used by
    /// `WasmiBlock::handle_with_attachments` so a wasmi callee's
    /// `__wafer_host_lookup_attachment` host import can find the attachments
    /// the caller provided via `Context::call_block_with_attachments`.
    async fn call_guest_resumable_with_attachments(
        &self,
        ctx: &dyn Context,
        attachments: Option<std::collections::BTreeMap<String, wafer_block::Attachment>>,
        setup: impl FnOnce(
            &mut Store<WasmiHostState>,
            wasmi::Instance,
        )
            -> Result<(wasmi::TypedFunc<(i32, i32), i64>, i32, i32), RuntimeError>,
    ) -> Result<Vec<u8>, RuntimeError> {
        let guard = ContextGuard::new(ctx);
        let caps_snapshot = self.capabilities.read().clone();
        let (mut store, instance) =
            instantiate(&self.engine, &self.linker, &self.module, &caps_snapshot)?;
        store.data_mut().context = Some(guard.as_arc());
        store.data_mut().current_attachments = attachments;

        let (func, arg0, arg1) = setup(&mut store, instance)?;

        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| RuntimeError::Wasm("guest has no exported memory".to_string()))?;

        // Initial call (resumable).
        let mut resumable = match func
            .call_resumable(&mut store, (arg0, arg1))
            .map_err(|e| RuntimeError::Wasm(format!("guest call failed: {e}")))?
        {
            TypedResumableCall::Finished(packed) => {
                let (ptr, len) = unpack_ptr_len(packed);
                let result = read_guest_bytes(&store, memory, ptr, len)?;
                // Drop context ref before guard.
                store.data_mut().context = None;
                return Ok(result);
            }
            TypedResumableCall::Resumable(inv) => inv,
        };

        // Resolve pending calls in a loop.
        loop {
            // Dispatch based on which pending field is set by the host import.
            if let Some(handle) = store.data_mut().pending_stream_finish.take() {
                // Pull the request out of the StreamState, dispatch via
                // Context::call_block, install the resulting OutputStream on
                // the StreamState. Resume with i32 0 on success, negative
                // ErrorCode on failure.
                // Drain (target, msg, body) and any attachments accumulated
                // via __wafer_host_stream_attach. Both come off the same
                // StreamState; the attachments hand-off must happen before we
                // await the dispatch (we don't want to keep `&mut store`
                // borrowed across an await).
                let take_result = {
                    let data = store.data_mut();
                    let state = data.streams.get_mut(handle);
                    state.map(|s| {
                        let req = s.take_finish_request();
                        let atts = s.take_attachments();
                        (req, atts)
                    })
                };
                let resume_code: i32 = match take_result {
                    Some((Ok((target, msg, body)), attachments)) => {
                        debug!(
                            block = target,
                            body_len = body.len(),
                            attachments = attachments.len(),
                            "resolving stream_finish from WASM guest"
                        );
                        let input = if body.is_empty() {
                            InputStream::empty()
                        } else {
                            InputStream::from_bytes(body)
                        };
                        let out = if attachments.is_empty() {
                            ctx.call_block(&target, msg, input).await
                        } else {
                            ctx.call_block_with_attachments(&target, msg, input, attachments)
                                .await
                        };
                        if let Some(state) = store.data_mut().streams.get_mut(handle) {
                            state.finish_with_stream(out);
                        }
                        0
                    }
                    Some((Err(e), _attachments)) => {
                        let code = e.code;
                        if let Some(state) = store.data_mut().streams.get_mut(handle) {
                            state.record_error_and_close(e);
                        }
                        error_code_to_neg_i32(code)
                    }
                    None => error_code_to_neg_i32(ErrorCode::NotFound),
                };

                match resumable
                    .resume(&mut store, &[Val::I32(resume_code)])
                    .map_err(|e| {
                        RuntimeError::Wasm(format!("resuming guest after stream_finish: {e}"))
                    })? {
                    TypedResumableCall::Finished(packed) => {
                        let (ptr, len) = unpack_ptr_len(packed);
                        let result = read_guest_bytes(&store, memory, ptr, len)?;
                        store.data_mut().context = None;
                        return Ok(result);
                    }
                    TypedResumableCall::Resumable(next) => {
                        resumable = next;
                    }
                }
            } else if let Some(handle) = store.data_mut().pending_stream_read.take() {
                // Drive the response stream's next frame. On Chunk: allocate
                // guest memory + write bytes, resume with packed (ptr, len).
                // On end-of-stream: resume with 0. On error: resume with
                // negative ErrorCode sentinel (the guest can call take_error
                // for full details).
                let next = match store.data_mut().streams.get_mut(handle) {
                    Some(s) => s.next_chunk().await,
                    None => Err(WaferError::new(
                        ErrorCode::NotFound,
                        "unknown stream handle",
                    )),
                };

                let resume_packed: i64 = match next {
                    Ok(Some(bytes)) => {
                        let alloc_fn = instance
                            .get_typed_func::<i32, i32>(&store, "__wafer_alloc")
                            .map_err(|e| {
                                RuntimeError::Wasm(format!(
                                    "getting __wafer_alloc for stream_read resume: {e}"
                                ))
                            })?;
                        let ptr = write_guest_bytes(&mut store, alloc_fn, memory, &bytes)?;
                        pack_ptr_len(ptr, bytes.len() as u32)
                    }
                    Ok(None) => 0,
                    Err(e) => error_code_to_neg_i64(e.code),
                };

                match resumable
                    .resume(&mut store, &[Val::I64(resume_packed)])
                    .map_err(|e| {
                        RuntimeError::Wasm(format!("resuming guest after stream_read: {e}"))
                    })? {
                    TypedResumableCall::Finished(packed) => {
                        let (ptr, len) = unpack_ptr_len(packed);
                        let result = read_guest_bytes(&store, memory, ptr, len)?;
                        store.data_mut().context = None;
                        return Ok(result);
                    }
                    TypedResumableCall::Resumable(next) => {
                        resumable = next;
                    }
                }
            } else if let Some(handle) = store.data_mut().pending_stream_take_error.take() {
                // Pop the StreamState's last_error, encode via rmp-serde,
                // allocate guest memory + write bytes, resume with packed
                // (ptr, len). Resume with 0 if no error is present.
                let err_opt = store
                    .data_mut()
                    .streams
                    .get_mut(handle)
                    .and_then(|s| s.take_error());

                let resume_packed: i64 = match err_opt {
                    Some(err) => {
                        let bytes = wafer_block::codec::encode(&err).map_err(|e| {
                            RuntimeError::Wasm(format!(
                                "encoding WaferError for stream_take_error: {e}"
                            ))
                        })?;
                        let alloc_fn = instance
                            .get_typed_func::<i32, i32>(&store, "__wafer_alloc")
                            .map_err(|e| {
                                RuntimeError::Wasm(format!(
                                    "getting __wafer_alloc for stream_take_error resume: {e}"
                                ))
                            })?;
                        let ptr = write_guest_bytes(&mut store, alloc_fn, memory, &bytes)?;
                        pack_ptr_len(ptr, bytes.len() as u32)
                    }
                    None => 0,
                };

                match resumable
                    .resume(&mut store, &[Val::I64(resume_packed)])
                    .map_err(|e| {
                        RuntimeError::Wasm(format!("resuming guest after stream_take_error: {e}"))
                    })? {
                    TypedResumableCall::Finished(packed) => {
                        let (ptr, len) = unpack_ptr_len(packed);
                        let result = read_guest_bytes(&store, memory, ptr, len)?;
                        store.data_mut().context = None;
                        return Ok(result);
                    }
                    TypedResumableCall::Resumable(next) => {
                        resumable = next;
                    }
                }
            } else if let Some(asset_id) = store.data_mut().pending_load_asset.take() {
                debug!(asset = asset_id, "resolving load_asset from WASM guest");

                let loader = self.asset_loader.read().clone();
                let status = loader.load(&asset_id).await;
                let code: i32 = match status {
                    crate::asset_loader::AssetLoadStatus::Ready => 0,
                    crate::asset_loader::AssetLoadStatus::Pending => 1,
                    crate::asset_loader::AssetLoadStatus::Failed(_) => 2,
                };

                // Resume with the status code as the return value of the
                // trapped host function. wasmi's resumable.resume value IS
                // the return value — no re-entry into the host fn.
                match resumable
                    .resume(&mut store, &[Val::I32(code)])
                    .map_err(|e| {
                        RuntimeError::Wasm(format!("resuming guest after load_asset: {e}"))
                    })? {
                    TypedResumableCall::Finished(packed) => {
                        let (ptr, len) = unpack_ptr_len(packed);
                        let result = read_guest_bytes(&store, memory, ptr, len)?;
                        store.data_mut().context = None;
                        return Ok(result);
                    }
                    TypedResumableCall::Resumable(next) => {
                        resumable = next;
                    }
                }
            } else {
                return Err(RuntimeError::Wasm(format!(
                    "guest trapped but no pending host call (host error: {})",
                    resumable.host_error()
                )));
            }
        }
    }

    fn warn_once_stripped_outbound(&self, names: &[String]) {
        use std::sync::atomic::Ordering;
        if self.warned_outbound.swap(true, Ordering::SeqCst) {
            return;
        }
        tracing::warn!(
            block = %self.info().name,
            direction = "outbound",
            stripped = ?names,
            "headers outside writable allowlist — stripped"
        );
    }

    fn warn_once_stripped_inbound(&self, names: &[String]) {
        use std::sync::atomic::Ordering;
        if self.warned_inbound.swap(true, Ordering::SeqCst) {
            return;
        }
        tracing::warn!(
            block = %self.info().name,
            direction = "inbound",
            stripped = ?names,
            "headers outside readable allowlist — stripped"
        );
    }
}

#[wafer_async_trait]
impl Block for WasmiBlock {
    fn info(&self) -> BlockInfo {
        // Check cache first.
        if let Ok(guard) = self.info_cache.lock() {
            if let Some(ref info) = *guard {
                return info.clone();
            }
        }

        // Sync instantiation: call __wafer_info.
        let result = (|| -> Result<BlockInfo, RuntimeError> {
            let caps_snapshot = self.capabilities.read().clone();
            let (mut store, instance) =
                instantiate(&self.engine, &self.linker, &self.module, &caps_snapshot)?;

            let info_fn = instance
                .get_typed_func::<(), i64>(&store, "__wafer_info")
                .map_err(|e| RuntimeError::Wasm(format!("getting __wafer_info: {e}")))?;

            let memory = instance
                .get_memory(&store, "memory")
                .ok_or_else(|| RuntimeError::Wasm("guest has no exported memory".to_string()))?;

            let packed = info_fn
                .call(&mut store, ())
                .map_err(|e| RuntimeError::Wasm(format!("calling __wafer_info: {e}")))?;

            let (ptr, len) = unpack_ptr_len(packed);
            let bytes = read_guest_bytes(&store, memory, ptr, len)?;
            let info: BlockInfo = serde_json::from_slice(&bytes)
                .map_err(|e| RuntimeError::Wasm(format!("deserializing BlockInfo: {e}")))?;
            Ok(info)
        })();

        match result {
            Ok(mut info) => {
                info.runtime = crate::block::BlockRuntime::Wasm;
                if let Ok(mut guard) = self.info_cache.lock() {
                    *guard = Some(info.clone());
                }
                info
            }
            Err(e) => {
                warn!("WasmiBlock::info() failed: {e}");
                BlockInfo::new("unknown", "0.0.0", "unknown", "failed to load info")
            }
        }
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        self.handle_inner(ctx, msg, input, None).await
    }

    async fn lifecycle(
        &self,
        ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        let event_bytes = serde_json::to_vec(&event).map_err(|e| {
            WaferError::new(
                ErrorCode::Internal,
                format!("serializing lifecycle event: {e}"),
            )
        })?;

        let result_bytes = self
            .call_guest_resumable(ctx, |store, instance| {
                let alloc_fn = instance
                    .get_typed_func::<i32, i32>(&*store, "__wafer_alloc")
                    .map_err(|e| RuntimeError::Wasm(format!("getting __wafer_alloc: {e}")))?;
                let lifecycle_fn = instance
                    .get_typed_func::<(i32, i32), i64>(&*store, "__wafer_lifecycle")
                    .map_err(|e| RuntimeError::Wasm(format!("getting __wafer_lifecycle: {e}")))?;
                let memory = instance.get_memory(&*store, "memory").ok_or_else(|| {
                    RuntimeError::Wasm("guest has no exported memory".to_string())
                })?;

                let ptr = write_guest_bytes(store, alloc_fn, memory, &event_bytes)?;
                let len = event_bytes.len() as i32;
                Ok((lifecycle_fn, ptr as i32, len))
            })
            .await
            .map_err(|e| {
                WaferError::new(ErrorCode::Internal, format!("WASM lifecycle error: {e}"))
            })?;

        // The guest returns a JSON-encoded Result<(), WaferError>.
        let result: std::result::Result<(), WaferError> = serde_json::from_slice(&result_bytes)
            .map_err(|e| {
                WaferError::new(
                    ErrorCode::Internal,
                    format!("deserializing WASM lifecycle result: {e}"),
                )
            })?;
        result
    }

    fn block_capabilities(&self) -> Option<BlockCapabilities> {
        Some(self.capabilities.read().clone())
    }

    fn runtime_capabilities_mut(&self, new: BlockCapabilities) {
        *self.capabilities.write() = new;
    }

    /// Expose `self` as `&dyn Any` so the runtime can downcast `Arc<dyn Block>`
    /// to `Arc<WasmiBlock>` and forward the host-side asset loader without
    /// importing `wafer-run` types into `wafer-block`.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
}

// ---------------------------------------------------------------------------
// Unit tests for interior-mutable capabilities update
// ---------------------------------------------------------------------------

#[cfg(test)]
mod capabilities_update_tests {
    use wafer_block::capabilities::BlockCapabilities;

    use super::*;

    /// Verify that `runtime_capabilities_mut` (via the Block trait) atomically
    /// replaces the internal capabilities and that subsequent calls to
    /// `block_capabilities()` reflect the new set.
    ///
    /// Loading a real WASM module is required to construct a WasmiBlock.
    /// We use the minimal WAT fixture from the fuel-exhaustion test — it has all
    /// required exports and exercises no guest logic.
    #[test]
    fn runtime_capabilities_mut_replaces_caps() {
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 1)
              (func (export "__wafer_alloc") (param i32) (result i32) i32.const 0)
              (func (export "__wafer_info") (result i64) i64.const 0)
              (func (export "__wafer_handle") (param i32 i32) (result i64) i64.const 0)
              (func (export "__wafer_lifecycle") (param i32 i32) (result i64) i64.const 0)
            )
            "#,
        )
        .expect("WAT should parse");

        // Load with unrestricted capabilities.
        let block =
            WasmiBlock::load_with_capabilities(&wasm_bytes, BlockCapabilities::unrestricted())
                .expect("minimal WAT module should load");

        // Confirm initial state: unrestricted → network = true.
        let before = block
            .block_capabilities()
            .expect("WasmiBlock must return Some(caps)");
        assert!(before.network, "initial caps should have network=true");

        // Apply a narrower capability set via the Block trait method.
        let narrowed = BlockCapabilities::none();
        use crate::block::Block;
        block.runtime_capabilities_mut(narrowed);

        // Confirm the update is visible.
        let after = block
            .block_capabilities()
            .expect("WasmiBlock must return Some(caps) after update");
        assert!(
            !after.network,
            "after runtime_capabilities_mut, network should be false"
        );
        assert!(
            !after.crypto,
            "after runtime_capabilities_mut, crypto should be false"
        );
        assert!(
            !after.raw_sql,
            "after runtime_capabilities_mut, raw_sql should be false"
        );
    }
}
