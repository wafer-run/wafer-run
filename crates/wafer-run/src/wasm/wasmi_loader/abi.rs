//! Low-level WASM ABI primitives shared by the host-import linker
//! ([`super::imports`]) and the [`super::WasmiBlock`] trap-resume dispatch:
//! packed pointer helpers, guest-memory I/O, error-code sentinels, the
//! trap+resume marker errors, and the per-store [`WasmiHostState`].

use std::sync::Arc;

use wasmi::Store;

use crate::{
    context::Context,
    error::RuntimeError,
    types::*,
    wasm::{capabilities::BlockCapabilities, stream::StreamRegistry},
};

/// Maximum WASM linear-memory pages (256 pages = 16 MiB).
const MAX_WASM_MEMORY_PAGES: usize = 256;

// ---------------------------------------------------------------------------
// Packed pointer helpers
// ---------------------------------------------------------------------------

/// Pack a (ptr, len) pair into a single i64 for the ABI.
pub(super) fn pack_ptr_len(ptr: u32, len: u32) -> i64 {
    ((ptr as i64) << 32) | (len as i64)
}

/// Unpack a packed i64 into (ptr, len).
///
/// A well-formed guest packs a non-negative `ptr << 32 | len`. A negative value
/// is a guest-side error sentinel (e.g. `error_code_to_neg_i64`) returned where
/// a packed pointer was expected, not a real `(ptr, len)` — splitting it would
/// hand `read_guest_bytes` a bogus offset/length. Reject it instead.
pub(super) fn unpack_ptr_len(packed: i64) -> Result<(u32, u32), RuntimeError> {
    if packed < 0 {
        return Err(RuntimeError::Wasm(format!(
            "guest returned a negative i64 ({packed}) where a packed (ptr, len) was expected \
             — likely an error sentinel returned from an export that must return a pointer"
        )));
    }
    let ptr = (packed >> 32) as u32;
    let len = (packed & 0xFFFF_FFFF) as u32;
    Ok((ptr, len))
}

// ---------------------------------------------------------------------------
// Host state stored in the wasmi Store
// ---------------------------------------------------------------------------

pub(super) struct WasmiHostState {
    /// Context reference — set before each guest call via ContextGuard.
    pub(super) context: Option<Arc<dyn Context>>,
    /// Capabilities (resource limits) for this block.
    /// Used by host function enforcement (e.g. `allows_call_block`).
    pub(super) capabilities: BlockCapabilities,
    /// Per-instance stream registry. Drops with the Store, cancelling any
    /// in-flight response streams via their paired `CancellationToken`s.
    pub(super) streams: StreamRegistry,
    /// Set by __wafer_host_stream_finish to request the host resume loop
    /// drive `Context::call_block` for this handle. The loop calls
    /// `take_finish_request` on the StreamState, dispatches, and installs
    /// the resulting OutputStream on the StreamState before resuming the
    /// guest with the i32 status code (0 = ok, negative = ErrorCode).
    pub(super) pending_stream_finish: Option<u64>,
    /// Set by __wafer_host_stream_read_chunk to request the host resume loop
    /// pull the next frame off the response stream. The loop allocates guest
    /// memory for the bytes (if any) and resumes with the packed (ptr, len)
    /// — or 0 for end-of-stream — or a negative ErrorCode sentinel.
    pub(super) pending_stream_read: Option<u64>,
    /// Set by __wafer_host_stream_take_error to request the host resume loop
    /// allocate guest memory and write the rmp-serde-encoded WaferError. The
    /// loop resumes with packed (ptr, len), or 0 if no error is present.
    pub(super) pending_stream_take_error: Option<u64>,
    /// Set by __wafer_host_load_asset to request an async asset load.
    /// The resume loop consumes this, drives the LoadAssetCallback, and
    /// resumes the guest with the resolved i32 status code as the return
    /// value (wasmi's `resumable.resume(..)` value IS the return value of
    /// the trapped host function — no phase-2 re-entry like call_block).
    pub(super) pending_load_asset: Option<String>,
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
pub(super) struct StreamFinishTrap;

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
pub(super) struct StreamReadTrap;

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
pub(super) struct StreamTakeErrorTrap;

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
pub(super) struct LoadAssetTrap;

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
/// host imports declared as `... -> i32`. The low byte carries the
/// `ErrorCode`'s stable ordinal (see [`ErrorCode::to_ordinal`]); the guest
/// unpacks via `take_error` for full details.
pub(super) fn error_code_to_neg_i32(code: ErrorCode) -> i32 {
    -(code.to_ordinal() as i32)
}

/// Negative-i64 variant. Same encoding as `error_code_to_neg_i32` but widened.
pub(super) fn error_code_to_neg_i64(code: ErrorCode) -> i64 {
    -(code.to_ordinal() as i64)
}

// ---------------------------------------------------------------------------
// Guest memory helpers
// ---------------------------------------------------------------------------

/// Read `len` bytes starting at `offset` from the guest's exported `memory`.
pub(super) fn read_guest_bytes(
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
pub(super) fn write_guest_bytes(
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
