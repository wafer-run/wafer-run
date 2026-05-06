//! Safe Rust wrappers around the streaming ABI host imports.
//!
//! Two types in two phases:
//!   - [`CallStream`] — request-writing phase. `write_chunk`* → `finish`.
//!   - [`ResponseStream`] — response-reading phase. `next_chunk`* → `drop`.
//!
//! Both have `Drop` guards that call `__wafer_host_stream_close`, so handle
//! leaks on panic are impossible.

use wafer_block::{ErrorCode, Message, WaferError};

#[cfg(target_arch = "wasm32")]
use crate::core_abi::{
    __wafer_host_stream_attach, __wafer_host_stream_close, __wafer_host_stream_finish,
    __wafer_host_stream_init, __wafer_host_stream_read_chunk, __wafer_host_stream_take_error,
    __wafer_host_stream_write_chunk,
};

// ---------------------------------------------------------------------------
// CallStream — request-writing phase
// ---------------------------------------------------------------------------

/// In-progress streaming call, request-writing phase.
///
/// Created by [`CallStream::open`]; transitions to [`ResponseStream`] via
/// [`CallStream::finish`]. The `Drop` guard closes the host handle if
/// `finish` was never called (e.g. due to an early return or panic).
pub struct CallStream {
    // Used only on wasm32 via unsafe host-import calls.
    #[allow(dead_code)]
    handle: i64,
    /// Set to `true` after a successful `finish`, so `Drop` does not
    /// double-close (ownership transfers to `ResponseStream`).
    finished: bool,
}

impl CallStream {
    /// Open a streaming call to `target_block`.
    ///
    /// Most callers won't use this directly — prefer the typed client wrappers
    /// in `wafer_sdk::clients` once they exist.
    pub fn open(target_block: &str, msg: &Message) -> Result<Self, WaferError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (target_block, msg);
            panic!("CallStream::open is only available in WASM blocks");
        }
        #[cfg(target_arch = "wasm32")]
        {
            let msg_bytes = wafer_block::codec::encode(msg)?;
            let handle = unsafe {
                __wafer_host_stream_init(
                    target_block.as_ptr() as i32,
                    target_block.len() as i32,
                    msg_bytes.as_ptr() as i32,
                    msg_bytes.len() as i32,
                )
            };
            if handle < 0 {
                let ordinal = ((-handle) & 0xFFFF_FFFF) as i32;
                let code = error_code_from_ordinal(ordinal);
                return Err(WaferError::new(
                    code,
                    format!("stream_init failed for {target_block}"),
                ));
            }
            Ok(Self {
                handle,
                finished: false,
            })
        }
    }

    /// Append a chunk to the request body.
    pub fn write_chunk(&mut self, chunk: &[u8]) -> Result<(), WaferError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = chunk;
            panic!("write_chunk is only available in WASM blocks");
        }
        #[cfg(target_arch = "wasm32")]
        {
            let r = unsafe {
                __wafer_host_stream_write_chunk(
                    self.handle,
                    chunk.as_ptr() as i32,
                    chunk.len() as i32,
                )
            };
            if r < 0 {
                let ordinal = (-r) as i32;
                let code = error_code_from_ordinal(ordinal);
                return Err(WaferError::new(code, "stream_write_chunk failed"));
            }
            Ok(())
        }
    }

    /// Attach a named binary blob to this stream's outgoing call. Must be
    /// called between `open` and `finish`.
    pub fn attach(
        &self,
        id: impl Into<String>,
        att: wafer_block::Attachment,
    ) -> Result<(), WaferError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (id, att);
            panic!("attach is only available in WASM blocks");
        }
        #[cfg(target_arch = "wasm32")]
        {
            let payload = wafer_block::codec::encode(&(id.into(), att))?;
            let r = unsafe {
                __wafer_host_stream_attach(
                    self.handle,
                    payload.as_ptr() as i32,
                    payload.len() as i32,
                )
            };
            if r < 0 {
                let ordinal = (-r) as i32;
                let code = error_code_from_ordinal(ordinal);
                return Err(WaferError::new(code, "stream_attach failed"));
            }
            Ok(())
        }
    }

    /// Close the request side and transition to reading the response.
    ///
    /// Ownership of the handle transfers to the returned [`ResponseStream`];
    /// this `CallStream` is consumed and its `Drop` is suppressed via
    /// `std::mem::forget`.
    pub fn finish(self) -> Result<ResponseStream, WaferError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            panic!("finish is only available in WASM blocks");
        }
        #[cfg(target_arch = "wasm32")]
        {
            let mut this = self;
            let r = unsafe { __wafer_host_stream_finish(this.handle) };
            if r < 0 {
                let ordinal = (-r) as i32;
                let code = error_code_from_ordinal(ordinal);
                return Err(WaferError::new(code, "stream_finish failed"));
            }
            // Mark finished so our Drop does not close the handle.
            this.finished = true;
            let handle = this.handle;
            // Prevent Drop from running — ownership transfers to ResponseStream.
            std::mem::forget(this);
            Ok(ResponseStream {
                handle,
                closed: false,
            })
        }
    }
}

impl Drop for CallStream {
    fn drop(&mut self) {
        if !self.finished {
            #[cfg(target_arch = "wasm32")]
            // SAFETY: handle is valid; close is idempotent on the host.
            unsafe {
                __wafer_host_stream_close(self.handle);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ResponseStream — response-reading phase
// ---------------------------------------------------------------------------

/// In-progress streaming call, response-reading phase.
///
/// Obtained by calling [`CallStream::finish`]. The `Drop` guard closes the
/// host handle automatically, so the caller need not call any explicit close.
pub struct ResponseStream {
    // Used only on wasm32 via unsafe host-import calls.
    #[allow(dead_code)]
    handle: i64,
    closed: bool,
}

impl ResponseStream {
    /// Pull the next response chunk.
    ///
    /// Returns:
    /// - `Ok(Some(bytes))` — a chunk is available.
    /// - `Ok(None)` — end of stream.
    /// - `Err(e)` — the host reported an error; `take_error` was called for
    ///   full details.
    pub fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, WaferError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            panic!("next_chunk is only available in WASM blocks");
        }
        #[cfg(target_arch = "wasm32")]
        // SAFETY: all host memory operations use pointers and lengths
        // returned by the host; we reconstruct Vecs for RAII cleanup.
        unsafe {
            let packed = __wafer_host_stream_read_chunk(self.handle);
            if packed == 0 {
                return Ok(None);
            }
            if packed < 0 {
                // Fetch full structured error via take_error.
                let err_packed = __wafer_host_stream_take_error(self.handle);
                if err_packed > 0 {
                    let (ptr, len) = crate::core_abi::unpack_ptr_len(err_packed);
                    let bytes = std::slice::from_raw_parts(ptr as *const u8, len as usize).to_vec();
                    // Free the host-allocated guest buffer.
                    drop(Vec::from_raw_parts(
                        ptr as *mut u8,
                        len as usize,
                        len as usize,
                    ));
                    let err: WaferError = wafer_block::codec::decode(&bytes).unwrap_or_else(|_| {
                        WaferError::new(ErrorCode::Internal, "stream error (undecodable)")
                    });
                    return Err(err);
                }
                return Err(WaferError::new(
                    ErrorCode::Internal,
                    "stream error (no detail)",
                ));
            }
            // Positive: packed (ptr, len) pointing to a guest-owned buffer.
            let (ptr, len) = crate::core_abi::unpack_ptr_len(packed);
            let bytes = std::slice::from_raw_parts(ptr as *const u8, len as usize).to_vec();
            // Free the host-allocated guest buffer.
            drop(Vec::from_raw_parts(
                ptr as *mut u8,
                len as usize,
                len as usize,
            ));
            Ok(Some(bytes))
        }
    }
}

impl Drop for ResponseStream {
    fn drop(&mut self) {
        if !self.closed {
            #[cfg(target_arch = "wasm32")]
            // SAFETY: handle is valid; close is idempotent on the host.
            unsafe {
                __wafer_host_stream_close(self.handle);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Error code mapping
// ---------------------------------------------------------------------------

/// Mirror of the host-side `error_code_ordinal` in
/// `crates/wafer-run/src/wasm/wasmi_loader.rs`.
///
/// Maps a small integer ordinal (the absolute value of the host's negative
/// sentinel) back to an `ErrorCode` variant. Unknown ordinals fall back to
/// `ErrorCode::Internal`.
///
/// **Must be kept in sync with the host mapping.** The host mapping is:
/// ```text
/// Ok=0, Cancelled=1, Unknown=2, InvalidArgument=3, DeadlineExceeded=4,
/// NotFound=5, AlreadyExists=6, PermissionDenied=7, ResourceExhausted=8,
/// FailedPrecondition=9, Aborted=10, OutOfRange=11, Unimplemented=12,
/// Internal=13, Unavailable=14, DataLoss=15, Unauthenticated=16
/// ```
#[allow(dead_code)]
pub(crate) fn error_code_from_ordinal(ordinal: i32) -> ErrorCode {
    // Normalise: host always emits positive ordinals as absolute values.
    let abs = if ordinal < 0 { -ordinal } else { ordinal };
    match abs {
        0 => ErrorCode::Ok,
        1 => ErrorCode::Cancelled,
        2 => ErrorCode::Unknown,
        3 => ErrorCode::InvalidArgument,
        4 => ErrorCode::DeadlineExceeded,
        5 => ErrorCode::NotFound,
        6 => ErrorCode::AlreadyExists,
        7 => ErrorCode::PermissionDenied,
        8 => ErrorCode::ResourceExhausted,
        9 => ErrorCode::FailedPrecondition,
        10 => ErrorCode::Aborted,
        11 => ErrorCode::OutOfRange,
        12 => ErrorCode::Unimplemented,
        13 => ErrorCode::Internal,
        14 => ErrorCode::Unavailable,
        15 => ErrorCode::DataLoss,
        16 => ErrorCode::Unauthenticated,
        _ => ErrorCode::Internal,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_abi::unpack_ptr_len;

    #[test]
    fn unpack_ptr_len_round_trips() {
        // Pack ptr=0x1000, len=0x100 → high 32 bits = 0x1000, low 32 bits = 0x100
        let packed = (0x1000_i64 << 32) | 0x100_i64;
        let (ptr, len) = unpack_ptr_len(packed);
        assert_eq!(ptr, 0x1000);
        assert_eq!(len, 0x100);
    }

    #[test]
    fn unpack_ptr_len_zero() {
        let (ptr, len) = unpack_ptr_len(0);
        assert_eq!(ptr, 0);
        assert_eq!(len, 0);
    }

    #[test]
    fn unpack_ptr_len_large_values() {
        let packed = (0xDEAD_BEEF_i64 << 32) | 0x1234_i64;
        let (ptr, len) = unpack_ptr_len(packed);
        assert_eq!(ptr, 0xDEAD_BEEF);
        assert_eq!(len, 0x1234);
    }

    #[test]
    fn error_code_from_ordinal_known_codes() {
        assert_eq!(error_code_from_ordinal(0), ErrorCode::Ok);
        assert_eq!(error_code_from_ordinal(1), ErrorCode::Cancelled);
        assert_eq!(error_code_from_ordinal(2), ErrorCode::Unknown);
        assert_eq!(error_code_from_ordinal(3), ErrorCode::InvalidArgument);
        assert_eq!(error_code_from_ordinal(4), ErrorCode::DeadlineExceeded);
        assert_eq!(error_code_from_ordinal(5), ErrorCode::NotFound);
        assert_eq!(error_code_from_ordinal(6), ErrorCode::AlreadyExists);
        assert_eq!(error_code_from_ordinal(7), ErrorCode::PermissionDenied);
        assert_eq!(error_code_from_ordinal(8), ErrorCode::ResourceExhausted);
        assert_eq!(error_code_from_ordinal(9), ErrorCode::FailedPrecondition);
        assert_eq!(error_code_from_ordinal(10), ErrorCode::Aborted);
        assert_eq!(error_code_from_ordinal(11), ErrorCode::OutOfRange);
        assert_eq!(error_code_from_ordinal(12), ErrorCode::Unimplemented);
        assert_eq!(error_code_from_ordinal(13), ErrorCode::Internal);
        assert_eq!(error_code_from_ordinal(14), ErrorCode::Unavailable);
        assert_eq!(error_code_from_ordinal(15), ErrorCode::DataLoss);
        assert_eq!(error_code_from_ordinal(16), ErrorCode::Unauthenticated);
    }

    #[test]
    fn error_code_from_ordinal_unknown_falls_back_to_internal() {
        assert_eq!(error_code_from_ordinal(99), ErrorCode::Internal);
        assert_eq!(error_code_from_ordinal(17), ErrorCode::Internal);
        assert_eq!(error_code_from_ordinal(255), ErrorCode::Internal);
    }

    #[test]
    fn error_code_from_ordinal_negative_input_normalised() {
        // Negative inputs should be treated as their absolute value.
        assert_eq!(error_code_from_ordinal(-3), ErrorCode::InvalidArgument);
        assert_eq!(error_code_from_ordinal(-13), ErrorCode::Internal);
        assert_eq!(error_code_from_ordinal(-16), ErrorCode::Unauthenticated);
    }
}
