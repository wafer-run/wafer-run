//! JSON ⇄ MessagePack transcoding for guests that negotiated
//! `HostCodec::Json` (see `wafer_block::abi::HOST_CODEC_EXPORT`).
//!
//! Wire DTOs are MessagePack *named maps* with plain `Vec<u8>` byte fields
//! (no `serde_bytes` in `wafer_block::wire`), so a lossless round trip
//! through `serde_json::Value` exists: bytes are integer arrays on both
//! sides and map keys are strings. Depth is bounded on both decoders.

use wafer_block::{ErrorCode, WaferError};

fn invalid(what: &str, e: impl std::fmt::Display) -> WaferError {
    WaferError::new(ErrorCode::InvalidArgument, format!("{what}: {e}"))
}

/// Transcode a JSON host-call body into the MessagePack named-map form the
/// callee's wire DTOs decode from. Applied to the request body of a
/// `HostCodec::Json` guest at `stream_finish`.
pub(super) fn json_to_rmp(json: &[u8]) -> Result<Vec<u8>, WaferError> {
    let value: serde_json::Value =
        serde_json::from_slice(json).map_err(|e| invalid("host-call body is not JSON", e))?;
    rmp_serde::to_vec_named(&value).map_err(|e| invalid("encoding host-call body", e))
}

/// Transcode a MessagePack response frame into JSON. Applied to every frame
/// read back by a `HostCodec::Json` guest at `stream_read_chunk`.
pub(super) fn rmp_to_json(rmp: &[u8]) -> Result<Vec<u8>, WaferError> {
    let mut de = rmp_serde::Deserializer::from_read_ref(rmp);
    de.set_max_depth(wafer_block::codec::WIRE_MAX_DEPTH);
    let value: serde_json::Value = serde::Deserialize::deserialize(&mut de)
        .map_err(|e| invalid("response frame is not MessagePack", e))?;
    // A frame carries exactly one encoded value. Anything after it — a second
    // value, or bytes that decode as nothing at all — means the frame is not
    // what the callee claims, so a clean end of input is the only acceptable
    // continuation. (`serde_json` already rejects trailing data on the way in,
    // so `json_to_rmp` needs no equivalent check.)
    match <serde::de::IgnoredAny as serde::Deserialize>::deserialize(&mut de) {
        Err(rmp_serde::decode::Error::InvalidMarkerRead(e))
            if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
        _ => {
            return Err(invalid(
                "response frame is not MessagePack",
                "trailing bytes after the encoded value",
            ))
        }
    }
    serde_json::to_vec(&value).map_err(|e| invalid("encoding response frame as JSON", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wafer_block::wire::database as wire;

    #[test]
    fn json_request_decodes_as_the_named_map_dto() {
        let json = br#"{"collection":"site__notes__items","data":{"id":"1","body":[104,105]}}"#;
        let rmp = json_to_rmp(json).unwrap();
        let req: wire::CreateRequest = wafer_block::codec::decode(&rmp).unwrap();
        assert_eq!(req.collection, "site__notes__items");
        assert_eq!(req.data["body"], serde_json::json!([104, 105]));
    }

    #[test]
    fn rmp_response_round_trips_bytes_as_integer_arrays() {
        let resp = wafer_block::wire::storage::GetResponse {
            data: vec![1, 2, 3],
            info: wafer_block::wire::storage::ObjectInfo {
                key: String::new(),
                size: 0,
                content_type: String::new(),
                last_modified: chrono::Utc::now(),
            },
        };
        let rmp = wafer_block::codec::encode(&resp).unwrap();
        let json = rmp_to_json(&rmp).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(v["data"], serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn malformed_json_is_invalid_argument() {
        let err = json_to_rmp(b"{not json").unwrap_err();
        assert_eq!(err.code, wafer_block::ErrorCode::InvalidArgument);
    }

    #[test]
    fn trailing_bytes_after_the_value_are_invalid_argument() {
        let rmp = wafer_block::codec::encode(&serde_json::json!({"a": 1})).unwrap();
        assert!(rmp_to_json(&rmp).is_ok(), "the value alone must decode");

        // A second, perfectly valid value appended to the frame.
        let mut two_values = rmp.clone();
        two_values.extend_from_slice(&wafer_block::codec::encode(&serde_json::json!(7)).unwrap());
        assert_eq!(
            rmp_to_json(&two_values).unwrap_err().code,
            wafer_block::ErrorCode::InvalidArgument,
        );

        // Junk that decodes as nothing at all (0xc1 is the reserved marker).
        let mut junk = rmp;
        junk.push(0xc1);
        assert_eq!(
            rmp_to_json(&junk).unwrap_err().code,
            wafer_block::ErrorCode::InvalidArgument,
        );
    }

    #[test]
    fn malformed_rmp_is_invalid_argument() {
        let err = rmp_to_json(&[0xc1]).unwrap_err();
        assert_eq!(err.code, wafer_block::ErrorCode::InvalidArgument);
    }
}
