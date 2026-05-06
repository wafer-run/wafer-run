//! Guest-side helper for `__wafer_host_lookup_attachment`.

use wafer_block::{Attachment, WaferError};

/// Look up an inbound attachment by id.
///
/// Returns `Ok(None)` for `NotFound` (the caller did not attach under this id);
/// `Ok(Some(_))` if found; `Err(_)` on any other host-reported error.
#[cfg(target_arch = "wasm32")]
pub fn lookup_attachment(id: &str) -> Result<Option<Attachment>, WaferError> {
    use crate::core_abi::{__wafer_host_lookup_attachment, unpack_ptr_len};
    use wafer_block::{codec, ErrorCode};

    let packed = unsafe { __wafer_host_lookup_attachment(id.as_ptr() as i32, id.len() as i32) };
    if packed < 0 {
        let ordinal = ((-packed) & 0xFFFF_FFFF) as i32;
        if ordinal == ErrorCode::NotFound as i32 {
            return Ok(None);
        }
        return Err(WaferError::new(
            ErrorCode::Internal,
            format!("lookup_attachment failed: code={ordinal}"),
        ));
    }
    let (ptr, len) = unpack_ptr_len(packed);
    // Reclaim the guest-allocated buffer. The host wrote `len` bytes into a
    // buffer allocated via __wafer_alloc with capacity == len; reconstructing
    // the Vec with capacity == len matches that allocation.
    let bytes = unsafe { Vec::from_raw_parts(ptr as *mut u8, len as usize, len as usize) };
    let att: Attachment = codec::decode(&bytes)?;
    Ok(Some(att))
}

/// Stub for non-wasm targets — panics on call. Maintains compile-time presence
/// so guest code can be checked by `cargo build` (without wasm32 target) without
/// needing `#[cfg(target_arch = "wasm32")]` decorators on every call site.
#[cfg(not(target_arch = "wasm32"))]
pub fn lookup_attachment(_id: &str) -> Result<Option<Attachment>, WaferError> {
    panic!("lookup_attachment is only available in WASM blocks")
}
