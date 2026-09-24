//! Guest-side helper for `__wafer_host_lookup_attachment`.

#[cfg(any(target_arch = "wasm32", test))]
use wafer_block::ErrorCode;
use wafer_block::{Attachment, WaferError};

#[cfg(any(target_arch = "wasm32", test))]
use crate::{core_abi::HostBuffer, stream::error_code_from_ordinal};

/// Look up an inbound attachment by id.
///
/// Returns `Ok(None)` for `NotFound` (the caller did not attach under this id);
/// `Ok(Some(_))` if found; `Err(_)` on any other host-reported error, or when
/// the host replies with a null buffer.
#[cfg(target_arch = "wasm32")]
pub fn lookup_attachment(id: &str) -> Result<Option<Attachment>, WaferError> {
    use wafer_block::codec;

    use crate::core_abi::__wafer_host_lookup_attachment;

    let packed = unsafe { __wafer_host_lookup_attachment(id.as_ptr() as i32, id.len() as i32) };
    let Some(buffer) = read_reply(packed)? else {
        return Ok(None);
    };
    // SAFETY: a non-null positive reply is a `__wafer_alloc(len)` buffer the
    // host filled with the encoded attachment and handed to this guest.
    let bytes = unsafe { buffer.into_vec() };
    let att: Attachment = codec::decode(&bytes)?;
    Ok(Some(att))
}

/// Interpret a `__wafer_host_lookup_attachment` reply: a negative `NotFound`
/// sentinel is `Ok(None)`, any other negative sentinel is that error, and a
/// non-negative value is the packed buffer, which must not be null.
#[cfg(any(target_arch = "wasm32", test))]
fn read_reply(packed: i64) -> Result<Option<HostBuffer>, WaferError> {
    if packed < 0 {
        let ordinal = (packed.unsigned_abs() & 0xFFFF_FFFF) as i32;
        let code = error_code_from_ordinal(ordinal);
        if code == ErrorCode::NotFound {
            return Ok(None);
        }
        return Err(WaferError::new(
            code,
            format!("lookup_attachment failed: code={code:?}"),
        ));
    }
    HostBuffer::from_packed(packed).map(Some).ok_or_else(|| {
        WaferError::new(
            ErrorCode::Internal,
            "lookup_attachment: the host returned a null buffer",
        )
    })
}

/// Stub for non-wasm targets — panics on call. Maintains compile-time presence
/// so guest code can be checked by `cargo build` (without wasm32 target) without
/// needing `#[cfg(target_arch = "wasm32")]` decorators on every call site.
#[cfg(not(target_arch = "wasm32"))]
pub fn lookup_attachment(_id: &str) -> Result<Option<Attachment>, WaferError> {
    panic!("lookup_attachment is only available in WASM blocks")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_abi::pack_ptr_len;

    fn sentinel(code: ErrorCode) -> i64 {
        -i64::from(code.to_ordinal())
    }

    #[test]
    fn not_found_sentinel_is_no_attachment() {
        assert_eq!(read_reply(sentinel(ErrorCode::NotFound)), Ok(None));
    }

    #[test]
    fn other_sentinels_are_errors() {
        let err = read_reply(sentinel(ErrorCode::InvalidArgument)).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn a_null_buffer_is_an_error_not_a_vec() {
        for packed in [0, pack_ptr_len(0, 8)] {
            let err = read_reply(packed).unwrap_err();
            assert_eq!(err.code, ErrorCode::Internal, "packed={packed:#x}");
        }
    }

    #[test]
    fn a_non_null_buffer_is_returned() {
        let buffer = read_reply(pack_ptr_len(4096, 12)).unwrap();
        assert_eq!(buffer, HostBuffer::from_packed(pack_ptr_len(4096, 12)));
    }
}
