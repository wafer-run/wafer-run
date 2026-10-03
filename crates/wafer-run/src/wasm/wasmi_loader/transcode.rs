//! JSON ⇄ MessagePack transcoding for guests that negotiated
//! `HostCodec::Json` (see `wafer_block::abi::HOST_CODEC_EXPORT`).
//!
//! Wire DTOs are MessagePack *named maps* with plain `Vec<u8>` byte fields
//! (no `serde_bytes` in `wafer_block::wire`), so a lossless transcode
//! exists: bytes are integer arrays on both sides and map keys are strings.
//!
//! The transcode streams one format's events straight into the other's
//! serializer (`serde_transcode`) rather than materializing a
//! `serde_json::Value`: a `Value` object sorts its keys, and a map's key
//! order is part of what some payloads say — a database row
//! (`wafer_block::wire::database::RecordData`) lists its columns in the order
//! the statement returned them, and a JSON guest must see that order too.
//! Depth is bounded on both decoders.

use wafer_block::{ErrorCode, WaferError};

fn invalid(what: &str, e: impl std::fmt::Display) -> WaferError {
    WaferError::new(ErrorCode::InvalidArgument, format!("{what}: {e}"))
}

/// Transcode a JSON host-call body into the MessagePack named-map form the
/// callee's wire DTOs decode from. Applied to the request body of a
/// `HostCodec::Json` guest at `stream_finish`.
///
/// Every failure is the body's: encoding a JSON value as MessagePack into a
/// `Vec` cannot fail, so an error out of the transcode is a JSON parse error
/// (the deserializer's, reported through the serializer's error type).
pub(super) fn json_to_rmp(json: &[u8]) -> Result<Vec<u8>, WaferError> {
    const NOT_JSON: &str = "host-call body is not JSON";
    let mut de = serde_json::Deserializer::from_slice(json);
    let mut rmp = Vec::with_capacity(json.len());
    serde_transcode::transcode(&mut de, &mut rmp_serde::Serializer::new(&mut rmp))
        .map_err(|e| invalid(NOT_JSON, e))?;
    // Only whitespace may follow the value.
    de.end().map_err(|e| invalid(NOT_JSON, e))?;
    Ok(rmp)
}

/// Transcode a MessagePack response frame into JSON. Applied to every frame
/// read back by a `HostCodec::Json` guest at `stream_read_chunk`.
pub(super) fn rmp_to_json(rmp: &[u8]) -> Result<Vec<u8>, WaferError> {
    const NOT_RMP: &str = "response frame is not MessagePack";
    let mut de = rmp_serde::Deserializer::from_read_ref(rmp);
    de.set_max_depth(wafer_block::codec::WIRE_MAX_DEPTH);
    let mut json = Vec::with_capacity(rmp.len());
    serde_transcode::transcode(&mut de, &mut serde_json::Serializer::new(&mut json))
        .map_err(|e| invalid(NOT_RMP, e))?;
    // A frame carries exactly one encoded value. Anything after it — a second
    // value, or bytes that decode as nothing at all — means the frame is not
    // what the callee claims, so a clean end of input is the only acceptable
    // continuation. (`json_to_rmp` checks its input's end the same way.)
    match <serde::de::IgnoredAny as serde::Deserialize>::deserialize(&mut de) {
        Err(rmp_serde::decode::Error::InvalidMarkerRead(e))
            if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
        _ => return Err(invalid(NOT_RMP, "trailing bytes after the encoded value")),
    }
    Ok(json)
}

#[cfg(test)]
mod tests {
    use wafer_block::wire::database as wire;

    use super::*;

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

    /// A `SELECT b, a, c` row, encoded by the host as a database response.
    fn bac_row() -> wire::Record {
        wire::Record {
            id: String::new(),
            data: wire::RecordData::from([
                ("b".to_string(), serde_json::json!(2)),
                ("a".to_string(), serde_json::json!(1)),
                ("c".to_string(), serde_json::json!(3)),
            ]),
        }
    }

    #[test]
    fn rmp_response_keeps_the_row_column_order() {
        let rmp = wafer_block::codec::encode(&bac_row()).unwrap();
        let json = rmp_to_json(&rmp).unwrap();
        assert_eq!(
            std::str::from_utf8(&json).unwrap(),
            r#"{"id":"","data":{"b":2,"a":1,"c":3}}"#,
        );
    }

    #[test]
    fn json_request_keeps_its_key_order() {
        let json = br#"{"id":"","data":{"b":2,"a":1,"c":3}}"#;
        let row: wire::Record = wafer_block::codec::decode(&json_to_rmp(json).unwrap()).unwrap();
        let names: Vec<&str> = row.data.keys().map(String::as_str).collect();
        assert_eq!(names, ["b", "a", "c"]);
    }

    #[test]
    fn trailing_json_after_the_value_is_invalid_argument() {
        assert!(
            json_to_rmp(br#"{"a":1}  "#).is_ok(),
            "trailing whitespace is fine"
        );
        assert_eq!(
            json_to_rmp(br#"{"a":1} 7"#).unwrap_err().code,
            wafer_block::ErrorCode::InvalidArgument,
        );
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

    /// A JSON guest has no raw request-body path: `json_to_rmp` runs over the
    /// WHOLE body, so opaque application bytes (what a streaming upload would
    /// write) are `InvalidArgument` rather than being passed through. See
    /// `wafer_block::abi::HOST_CODEC_EXPORT`.
    #[test]
    fn raw_request_body_bytes_are_invalid_argument() {
        // A PNG header — perfectly valid upload bytes, not valid JSON.
        let err = json_to_rmp(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]).unwrap_err();
        assert_eq!(err.code, wafer_block::ErrorCode::InvalidArgument);
        assert!(
            err.message.contains("host-call body is not JSON"),
            "the refusal must say the body could not be read, got: {}",
            err.message
        );
    }

    #[test]
    fn malformed_rmp_is_invalid_argument() {
        let err = rmp_to_json(&[0xc1]).unwrap_err();
        assert_eq!(err.code, wafer_block::ErrorCode::InvalidArgument);
    }
}
