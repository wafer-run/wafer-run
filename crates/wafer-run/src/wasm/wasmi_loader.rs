use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tracing::{debug, warn};
use wafer_block::streams::{input::InputStream, output::OutputStream};
use wasmi::{Caller, Engine, Error as WasmiError, Linker, Module, Store, TypedResumableCall, Val};

use super::{capabilities::BlockCapabilities, host::ContextGuard};
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
    /// Set by __wafer_host_call_block to request an async call.
    /// The resume loop in `call_guest_resumable` consumes this, dispatches
    /// the call via `Context::call_block`, allocates guest memory for the
    /// resulting GuestResult JSON, writes it, and resumes the guest with
    /// the packed (ptr, len) as the host call's return value (no re-entry).
    pending_call: Option<PendingCall>,
    /// Set by __wafer_host_load_asset to request an async asset load.
    /// The resume loop consumes this, drives the LoadAssetCallback, and
    /// resumes the guest with the resolved i32 status code as the return
    /// value (wasmi's `resumable.resume(..)` value IS the return value of
    /// the trapped host function — no phase-2 re-entry like call_block).
    pending_load_asset: Option<String>,
}

struct PendingCall {
    block_name: String,
    msg_bytes: Vec<u8>,
    body_bytes: Vec<u8>,
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
// Sentinel error for call_block trap+resume
// ---------------------------------------------------------------------------

/// Marker error returned by `__wafer_host_call_block` to suspend execution.
/// wasmi's resumable-call machinery catches this and lets the host resolve
/// the async call before resuming.
#[derive(Debug)]
struct CallBlockTrap;

impl std::fmt::Display for CallBlockTrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "call_block trap (expected — will be resumed)")
    }
}

impl wasmi::core::HostError for CallBlockTrap {}

/// Marker error returned by `__wafer_host_load_asset` to suspend execution.
/// Mirrors `CallBlockTrap` — the resume loop catches it and drives the
/// registered `LoadAssetCallback` before resuming the guest.
#[derive(Debug)]
struct LoadAssetTrap;

impl std::fmt::Display for LoadAssetTrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "load_asset trap (expected — will be resumed)")
    }
}

impl wasmi::core::HostError for LoadAssetTrap {}

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

    // __wafer_host_call_block(name_ptr, name_len, msg_ptr, msg_len, body_ptr, body_len) -> i64
    //
    // Reads the call args (incl. request body) from guest memory, stores a
    // PendingCall, and traps. The resume loop in `call_guest_resumable`
    // catches the trap, dispatches to the target block via `Context::call_block`
    // (passing the request body bytes as the target's InputStream), allocates
    // guest memory for the response GuestResult JSON, writes it, and resumes
    // the guest with the packed (ptr, len) as the host call's return value.
    //
    // The request body is forwarded to the target as InputStream
    // (symmetric with `handle(msg, body)`); the response body is encoded
    // inside the GuestResult JSON returned from this call (see the `Respond`
    // payload's `response.data` field).
    linker
        .func_wrap(
            "wafer",
            "__wafer_host_call_block",
            |mut caller: Caller<WasmiHostState>,
             name_ptr: i32,
             name_len: i32,
             msg_ptr: i32,
             msg_len: i32,
             body_ptr: i32,
             body_len: i32|
             -> Result<i64, WasmiError> {
                // Read block name, message, and body from guest memory, then trap.
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

                // Capability check: deny if block is not allowed to call the target.
                if !caller.data().capabilities.allows_call_block(&block_name) {
                    return Err(WasmiError::new(format!(
                        "call_block to '{block_name}' denied by block capabilities"
                    )));
                }

                let mut msg_buf = vec![0u8; msg_len as usize];
                memory
                    .read(&caller, msg_ptr as usize, &mut msg_buf)
                    .map_err(|e| WasmiError::new(format!("reading call_block message: {e}")))?;

                let mut body_buf = vec![0u8; body_len as usize];
                if body_len > 0 {
                    memory
                        .read(&caller, body_ptr as usize, &mut body_buf)
                        .map_err(|e| WasmiError::new(format!("reading call_block body: {e}")))?;
                }

                // Store the pending call and trap to yield control.
                caller.data_mut().pending_call = Some(PendingCall {
                    block_name,
                    msg_bytes: msg_buf,
                    body_bytes: body_buf,
                });
                Err(WasmiError::host(CallBlockTrap))
            },
        )
        .map_err(|e| RuntimeError::Wasm(format!("linking __wafer_host_call_block: {e}")))?;

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
    // TinyGo WASM runtime imports this for time.Now() etc.
    // We return 0 nanoseconds — blocks should not rely on wall-clock time.
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "clock_time_get",
            |mut caller: Caller<WasmiHostState>, _id: i32, _precision: i64, time_ptr: i32| -> i32 {
                // Write 0u64 (nanoseconds) at time_ptr.
                if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                    let _ = memory.write(&mut caller, time_ptr as usize, &0u64.to_le_bytes());
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
        pending_call: None,
        pending_load_asset: None,
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
        pending_call: None,
        pending_load_asset: None,
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
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load(path: &str) -> Result<Self, RuntimeError> {
        let bytes = std::fs::read(path)
            .map_err(|e| RuntimeError::Wasm(format!("reading WASM file: {e}")))?;
        Self::load_from_bytes(&bytes)
    }

    pub fn load_from_bytes(wasm_bytes: &[u8]) -> Result<Self, RuntimeError> {
        Self::load_with_capabilities(wasm_bytes, BlockCapabilities::unrestricted())
    }

    pub fn load_with_capabilities(
        wasm_bytes: &[u8],
        caps: BlockCapabilities,
    ) -> Result<Self, RuntimeError> {
        let mut config = wasmi::Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        Self::load_with_engine(&engine, wasm_bytes, caps)
    }

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
        let guard = ContextGuard::new(ctx);
        let caps_snapshot = self.capabilities.read().clone();
        let (mut store, instance) =
            instantiate(&self.engine, &self.linker, &self.module, &caps_snapshot)?;
        store.data_mut().context = Some(guard.as_arc());

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
            if let Some(pending) = store.data_mut().pending_call.take() {
                debug!(
                    block = pending.block_name,
                    msg_len = pending.msg_bytes.len(),
                    body_len = pending.body_bytes.len(),
                    "resolving call_block from WASM guest"
                );

                // Deserialize the message, call the block with the
                // guest-supplied body as InputStream, collect and serialize
                // the result.
                let msg: Message = serde_json::from_slice(&pending.msg_bytes).map_err(|e| {
                    RuntimeError::Wasm(format!("deserializing call_block message: {e}"))
                })?;

                let input = if pending.body_bytes.is_empty() {
                    InputStream::empty()
                } else {
                    InputStream::from_bytes(pending.body_bytes)
                };

                let out = ctx.call_block(&pending.block_name, msg, input).await;
                let buf = out.collect_buffered().await;
                // Serialize the outcome for the WASM guest.
                // The guest expects a guest ABI format JSON with action/response/error.
                // We map BufferedResponse → respond, Error → error terminal.
                use wafer_block::streams::output::TerminalNotResponse;
                let result_bytes = match buf {
                    Ok(response) => {
                        // Build a guest ABI format response JSON the WASM guest can decode
                        let payload = serde_json::json!({
                            "action": "Respond",
                            "response": { "data": response.body, "meta": response.meta },
                            "error": null,
                            "message": null
                        });
                        serde_json::to_vec(&payload).map_err(|e| {
                            RuntimeError::Wasm(format!("serializing call_block result: {e}"))
                        })?
                    }
                    Err(TerminalNotResponse::Error(e)) => {
                        let payload = serde_json::json!({
                            "action": "Error",
                            "response": null,
                            "error": { "code": format!("{:?}", e.code), "message": e.message, "meta": [] },
                            "message": null
                        });
                        serde_json::to_vec(&payload).map_err(|e| {
                            RuntimeError::Wasm(format!("serializing call_block error: {e}"))
                        })?
                    }
                    Err(TerminalNotResponse::Drop) => {
                        let payload = serde_json::json!({
                            "action": "Drop",
                            "response": null,
                            "error": null,
                            "message": null
                        });
                        serde_json::to_vec(&payload).map_err(|e| {
                            RuntimeError::Wasm(format!("serializing call_block drop: {e}"))
                        })?
                    }
                    Err(TerminalNotResponse::Continue(next_msg)) => {
                        let payload = serde_json::json!({
                            "action": "Continue",
                            "response": null,
                            "error": null,
                            "message": next_msg
                        });
                        serde_json::to_vec(&payload).map_err(|e| {
                            RuntimeError::Wasm(format!("serializing call_block continue: {e}"))
                        })?
                    }
                    Err(TerminalNotResponse::Malformed) => {
                        let payload = serde_json::json!({
                            "action": "Error",
                            "response": null,
                            "error": { "code": "Internal", "message": "malformed output stream", "meta": [] },
                            "message": null
                        });
                        serde_json::to_vec(&payload).map_err(|e| {
                            RuntimeError::Wasm(format!("serializing malformed error: {e}"))
                        })?
                    }
                };

                // Allocate guest memory via __wafer_alloc and write the
                // GuestResult JSON there, then resume with the packed
                // (ptr, len) as the host call's return value. wasmi's
                // resumable.resume value IS the return value of the
                // trapped host function — there is no re-entry, so we
                // must materialize the response bytes in guest memory
                // before resuming.
                let alloc_fn = instance
                    .get_typed_func::<i32, i32>(&store, "__wafer_alloc")
                    .map_err(|e| {
                        RuntimeError::Wasm(format!(
                            "getting __wafer_alloc for call_block resume: {e}"
                        ))
                    })?;
                let result_ptr = write_guest_bytes(&mut store, alloc_fn, memory, &result_bytes)?;
                let packed = pack_ptr_len(result_ptr, result_bytes.len() as u32);

                match resumable
                    .resume(&mut store, &[Val::I64(packed)])
                    .map_err(|e| {
                        RuntimeError::Wasm(format!("resuming guest after call_block: {e}"))
                    })? {
                    TypedResumableCall::Finished(packed) => {
                        let (ptr, len) = unpack_ptr_len(packed);
                        let result = read_guest_bytes(&store, memory, ptr, len)?;
                        store.data_mut().context = None;
                        return Ok(result);
                    }
                    TypedResumableCall::Resumable(next) => {
                        resumable = next;
                        // Continue the loop: another call_block from the same invocation.
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

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
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
        // Combine message + input body into a single JSON payload for the WASM guest.
        // The guest's __wafer_handle expects serialized Message bytes.
        // We collect the input stream and embed it into a wrapper so the guest
        // can decode both the message and the body.
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
            .call_guest_resumable(ctx, |store, instance| {
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
