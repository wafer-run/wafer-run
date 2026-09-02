//! Low-level WASM ABI primitives shared by the host-import linker
//! ([`super::imports`]) and the [`super::WasmiBlock`] trap-resume dispatch:
//! packed pointer helpers, guest-memory I/O, error-code sentinels, the
//! trap+resume marker errors, and the per-store [`WasmiHostState`].

use std::sync::Arc;

use wafer_block::{core_types::*, error::RuntimeError};
use wasmi::Store;

use super::codec::HostCodec;
use crate::{
    context::Context,
    wasm::{capabilities::BlockCapabilities, stream::StreamRegistry},
};

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
    /// Owned context handle — installed before each guest call via
    /// `ContextScope` (`Context::clone_arc`), cleared when the scope drops.
    pub(super) context: Option<Arc<dyn Context>>,
    /// Maximum WASM linear-memory size for this store, in 64 KiB pages. The
    /// [`wasmi::ResourceLimiter`] impl denies any `memory.grow` that would
    /// exceed it. Seeded at [`instantiate`](super::instantiate) from the
    /// block's configured [`ResourceLimits`](crate::ResourceLimits); defaults
    /// to [`DEFAULT_MAX_WASM_MEMORY_PAGES`](crate::DEFAULT_MAX_WASM_MEMORY_PAGES)
    /// (256 pages = 16 MiB).
    pub(super) max_memory_pages: u32,
    /// Maximum WebAssembly table elements for this store. The
    /// [`wasmi::ResourceLimiter`] impl denies any `table.grow` beyond it
    /// (SEC-03). Seeded at [`instantiate`](super::instantiate) from the block's
    /// configured [`ResourceLimits`](crate::ResourceLimits); defaults to
    /// [`DEFAULT_MAX_TABLE_ELEMENTS`](crate::runtime::wasm_state::DEFAULT_MAX_TABLE_ELEMENTS).
    pub(super) max_table_elements: u32,
    /// Capabilities (resource limits) for this block.
    /// Used by host function enforcement (e.g. `allows_call_block`).
    pub(super) capabilities: BlockCapabilities,
    /// SEC-01: the host-owned protected metadata entries (`auth.*`) from the
    /// message this guest was invoked with — the identity established upstream
    /// by the trusted host / auth middleware. Seeded per call; used to restore
    /// host ownership of the namespace on any nested `call_block` the guest
    /// initiates via the streaming ABI, so the guest cannot forge identity for
    /// a callee.
    pub(super) inbound_protected_meta: Vec<MetaEntry>,
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
    /// Codec of host-call payloads for this instance, negotiated once via
    /// `__wafer_host_codec` in [`instantiate`](super::instantiate). See
    /// `wafer_block::abi::HOST_CODEC_EXPORT`.
    pub(super) host_codec: HostCodec,
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
        Ok(desired_pages <= self.max_memory_pages as usize)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, wasmi::errors::TableError> {
        // SEC-03: bound table growth. Previously unconditionally `Ok(true)`,
        // so the linear-memory cap did not constrain WebAssembly tables.
        Ok(desired <= self.max_table_elements as usize)
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

/// Marker trap raised by the `proc_exit(code)` WASI stub. TinyGo's `_start`
/// terminates by calling `proc_exit`, so this is the expected end-of-`_start`
/// signal. The instantiation path downcasts the wasmi error to this marker via
/// [`wasmi::Error::downcast_ref`] — `code == 0` is a clean shutdown; a non-zero
/// `code` is surfaced as a guest error. Carrying the typed code (rather than
/// keying off the trap's `Display` string) is what makes that decision robust.
#[derive(Debug)]
pub(super) struct ProcExitTrap {
    /// The exit status the guest passed to `proc_exit`.
    pub(super) code: i32,
}

impl std::fmt::Display for ProcExitTrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "guest called proc_exit({})", self.code)
    }
}

impl wasmi::core::HostError for ProcExitTrap {}

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
//
// Every host import that turns a guest-supplied `(ptr, len)` pair into a
// host allocation MUST validate the range through `checked_guest_range` (or
// its `read_guest_slice` wrapper) before allocating anything. `ptr`/`len`
// arrive straight off the wasm stack as `i32`s fully controlled by the
// guest:
//
//   - A negative `len` cast blindly to `usize` sign-extends to a value near
//     `usize::MAX`. Sizing a `vec![0u8; len as usize]` from that panics with
//     a capacity overflow — on native that's an unwind, but on wasm32 (this
//     runtime is itself compiled to wasm32 for the browser build) a panic aborts
//     the whole embedding process.
//   - Even a well-formed non-negative `len`/`ptr` can run past the end of
//     the guest's own linear memory, or be huge enough on its own to force
//     a multi-GiB host allocation for a guest whose real memory is a few
//     pages — a memory-amplification DoS.
//
// Both must be rejected *before* anything is allocated, which is why the
// helpers below only ever call `Memory::data_size` (no allocation) until the
// range is known to be valid.

/// Validate a guest-supplied `(ptr, len)` byte range against the current
/// size of `memory`, without allocating anything. Returns the range as
/// `(start, len)` in host `usize`s on success.
///
/// Rejects `len < 0` outright, then rejects `ptr + len` overflowing or
/// exceeding `memory.data_size()`. `ptr`/`len` are reinterpreted as
/// unsigned 32-bit wasm addresses (`as u32`) before widening to `u64` for
/// the range arithmetic, so a negative bit pattern cannot wrap the check
/// via signed-to-unsigned sign extension.
pub(super) fn checked_guest_range<C: wasmi::AsContext>(
    ctx: &C,
    memory: wasmi::Memory,
    ptr: i32,
    len: i32,
) -> Result<(usize, usize), wasmi::Error> {
    if len < 0 {
        return Err(wasmi::Error::new(format!(
            "invalid length {len}: guest-supplied lengths must be non-negative"
        )));
    }
    let ptr_u64 = u64::from(ptr as u32);
    let len_u64 = u64::from(len as u32);
    let mem_size = memory.data_size(ctx) as u64;
    ptr_u64
        .checked_add(len_u64)
        .filter(|end| *end <= mem_size)
        .ok_or_else(|| {
            wasmi::Error::new(format!(
                "guest memory access out of bounds: ptr={ptr_u64} len={len_u64} \
                 exceeds memory size {mem_size}"
            ))
        })?;
    Ok((ptr_u64 as usize, len_u64 as usize))
}

/// Copy `len` bytes at `ptr` out of the guest's exported linear memory,
/// validating the range first via [`checked_guest_range`]. Reads via a
/// subslice of `memory.data(ctx)` — bounds-checked for free, and avoids the
/// zeroed intermediate allocation a `vec![0u8; len]` + `memory.read` pair
/// would otherwise force.
pub(super) fn read_guest_slice<C: wasmi::AsContext>(
    ctx: &C,
    memory: wasmi::Memory,
    ptr: i32,
    len: i32,
) -> Result<Vec<u8>, wasmi::Error> {
    let (start, len) = checked_guest_range(ctx, memory, ptr, len)?;
    Ok(memory.data(ctx)[start..start + len].to_vec())
}

/// Read `len` bytes starting at `offset` from the guest's exported `memory`.
pub(super) fn read_guest_bytes(
    store: &Store<WasmiHostState>,
    memory: wasmi::Memory,
    offset: u32,
    len: u32,
) -> Result<Vec<u8>, RuntimeError> {
    read_guest_slice(store, memory, offset as i32, len as i32)
        .map_err(|e| RuntimeError::Wasm(format!("reading guest memory at {offset}+{len}: {e}")))
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use wasmi::{Engine, Memory, MemoryType, Store};

    use super::*;

    /// Build a bare `Store<()>` + `Memory` pair sized to `bytes` (rounded up
    /// to the nearest 64 KiB wasm page) — no `WasmiHostState` or guest module
    /// needed since `checked_guest_range`/`read_guest_slice` only touch the
    /// `Memory` and a generic `AsContext`.
    fn test_memory_with_size(bytes: u32) -> (Store<()>, Memory) {
        let engine = Engine::default();
        let mut store = Store::new(&engine, ());
        let pages = bytes.div_ceil(65536);
        let ty = MemoryType::new(pages, Some(pages)).expect("valid memory type");
        let memory = Memory::new(&mut store, ty).expect("memory allocation");
        (store, memory)
    }

    fn host_state_with_table_cap(max_table_elements: u32) -> WasmiHostState {
        WasmiHostState {
            context: None,
            max_memory_pages: 256,
            max_table_elements,
            capabilities: BlockCapabilities::none(),
            inbound_protected_meta: Vec::new(),
            streams: StreamRegistry::new(),
            pending_stream_finish: None,
            pending_stream_read: None,
            pending_stream_take_error: None,
            pending_load_asset: None,
            current_attachments: None,
            host_codec: HostCodec::Rmp,
        }
    }

    // SEC-03: table growth is bounded (was unconditionally `Ok(true)`, so the
    // linear-memory cap did not constrain WebAssembly tables).
    #[test]
    fn table_growing_is_bounded_by_max_table_elements() {
        use wasmi::ResourceLimiter;
        let mut state = host_state_with_table_cap(100);
        assert!(
            state.table_growing(0, 100, None).unwrap(),
            "growing to exactly the cap is allowed"
        );
        assert!(
            !state.table_growing(0, 101, None).unwrap(),
            "growing past the cap is denied"
        );
    }

    #[test]
    fn read_guest_slice_rejects_negative_length() {
        // A negative i32 length sign-extends to a near-usize::MAX capacity and
        // panics `vec![0u8; ..]` with capacity overflow — on wasm32 deployments
        // that aborts the whole runtime. Reject before allocating.
        let (store, memory) = test_memory_with_size(16 * 65536); // 16 MiB guest memory
        let err = read_guest_slice(&store, memory, /* ptr */ 0, /* len */ -1)
            .expect_err("negative length must be rejected, not allocated");
        assert!(
            err.to_string().contains("invalid length"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn read_guest_slice_rejects_len_past_memory_size() {
        let (store, memory) = test_memory_with_size(16 * 65536);
        let err = read_guest_slice(&store, memory, /* ptr */ 0, /* len */ i32::MAX)
            .expect_err("length exceeding guest memory must be rejected pre-alloc");
        assert!(
            err.to_string().contains("out of bounds"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn read_guest_slice_rejects_ptr_plus_len_past_memory_size() {
        // Individually in-bounds ptr and len whose sum overflows the memory
        // size must also be rejected pre-alloc (not just len alone).
        let (store, memory) = test_memory_with_size(65536); // 1 page = 64 KiB
        let err = read_guest_slice(&store, memory, /* ptr */ 65530, /* len */ 100)
            .expect_err("ptr+len past memory size must be rejected");
        assert!(
            err.to_string().contains("out of bounds"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn read_guest_slice_reads_valid_range() {
        let (mut store, memory) = test_memory_with_size(65536);
        memory
            .write(&mut store, 4, &[1, 2, 3, 4])
            .expect("write within bounds");
        let bytes = read_guest_slice(&store, memory, 4, 4).expect("in-bounds read must succeed");
        assert_eq!(bytes, vec![1, 2, 3, 4]);
    }

    #[test]
    fn checked_guest_range_rejects_negative_length() {
        let (store, memory) = test_memory_with_size(65536);
        let err = checked_guest_range(&store, memory, 0, -1)
            .expect_err("negative length must be rejected, not allocated");
        assert!(
            err.to_string().contains("invalid length"),
            "unexpected error message: {err}"
        );
    }
}
