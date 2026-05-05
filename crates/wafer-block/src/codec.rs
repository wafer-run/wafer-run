//! Wire-format encoder/decoder. Single source of truth for the format choice.
//!
//! Today: MessagePack via `rmp-serde`. If the format ever changes (e.g. to
//! postcard or CBOR), only this file changes; `wire::*` types and consumers
//! are unaffected.

use serde::{de::DeserializeOwned, Serialize};

use crate::{ErrorCode, WaferError};

pub fn encode<T: Serialize + ?Sized>(v: &T) -> Result<Vec<u8>, WaferError> {
    // `to_vec_named` (not `to_vec`) encodes structs as named maps rather than
    // positional arrays. This keeps the wire format forward-compatible:
    // adding a field to a struct in a newer version doesn't break older
    // decoders, and removing/reordering fields stays decodable as long as
    // the named entries match. Locked in by the
    // `unknown_field_is_forward_compatible` test below.
    rmp_serde::to_vec_named(v).map_err(|e| {
        WaferError::new(
            ErrorCode::INTERNAL,
            format!("codec encode error in {}: {e}", std::any::type_name::<T>()),
        )
    })
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, WaferError> {
    rmp_serde::from_slice(bytes).map_err(|e| {
        WaferError::new(
            ErrorCode::INTERNAL,
            format!("codec decode error in {}: {e}", std::any::type_name::<T>()),
        )
    })
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Sample {
        text: String,
        bytes: Vec<u8>,
        count: u32,
    }

    #[test]
    fn round_trip_preserves_value() {
        let original = Sample {
            text: "hello".into(),
            bytes: vec![1, 2, 3, 255],
            count: 42,
        };
        let encoded = encode(&original).expect("encode");
        let decoded: Sample = decode(&encoded).expect("decode");
        assert_eq!(original, decoded);
    }

    #[test]
    fn vec_u8_does_not_inflate() {
        // The load-bearing invariant: a 1 MiB Vec<u8> encodes to ~1 MiB,
        // not ~6 MiB as JSON would.
        let sample = Sample {
            text: "x".into(),
            bytes: vec![0u8; 1024 * 1024],
            count: 0,
        };
        let encoded = encode(&sample).expect("encode");
        // 1 MiB body + small framing for {text, count}. Allow 5% headroom.
        assert!(
            encoded.len() < 1024 * 1024 + (1024 * 1024 / 20),
            "encoded size {} exceeds 1.05 MiB — Vec<u8> inflation regressed",
            encoded.len()
        );
    }

    #[test]
    fn unknown_field_is_forward_compatible() {
        // A "future" struct with an extra field should decode into the older
        // shape without error. Demonstrates the no-deny_unknown_fields stance.
        #[derive(Serialize)]
        struct FutureSample {
            text: String,
            bytes: Vec<u8>,
            count: u32,
            extra: String, // future field
        }
        let future = FutureSample {
            text: "x".into(),
            bytes: vec![],
            count: 1,
            extra: "added later".into(),
        };
        let encoded = encode(&future).expect("encode future");
        let decoded: Sample = decode(&encoded).expect("decode into older shape");
        assert_eq!(decoded.text, "x");
        assert_eq!(decoded.count, 1);
    }

    #[test]
    fn malformed_bytes_return_error() {
        let bad = vec![0xff, 0xff, 0xff, 0xff];
        let result: Result<Sample, _> = decode(&bad);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(format!("{err:?}").contains("codec decode error"));
    }
}
