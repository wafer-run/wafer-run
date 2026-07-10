//! Wire-format encoder/decoder. Single source of truth for the format choice.
//!
//! Today: MessagePack via `rmp-serde`. If the format ever changes (e.g. to
//! postcard or CBOR), only this file changes; `wire::*` types and consumers
//! are unaffected.

use serde::{de::DeserializeOwned, Serialize};

use crate::{ErrorCode, WaferError};

/// Maximum container-nesting depth accepted when decoding a wire body.
///
/// This must be far below rmp-serde's default depth limit (1024): decoding
/// recursion costs ~2KB+ of stack per level for `serde_json::Value`-bearing
/// types, so 1024 levels overflows a 2MB thread stack — an uncatchable abort
/// — before that limit ever fires (I-1 audit finding). 128 matches
/// serde_json's default recursion limit (anything that legitimately entered
/// through a JSON HTTP edge fits) and keeps worst-case decode stack use in
/// the low hundreds of KB, safe on small wasm32 stacks. Real wire payloads
/// are far shallower (`FilterNode` is bounded at depth 16 post-decode).
pub const WIRE_MAX_DEPTH: usize = 128;

/// Encode `v` as MessagePack bytes using named-map field encoding for forward
/// compatibility. Returns a [`WaferError`] with [`ErrorCode::Internal`] on
/// serialization failure.
pub fn encode<T: Serialize + ?Sized>(v: &T) -> Result<Vec<u8>, WaferError> {
    // `to_vec_named` (not `to_vec`) encodes structs as named maps rather than
    // positional arrays. This keeps the wire format forward-compatible:
    // adding a field to a struct in a newer version doesn't break older
    // decoders, and removing/reordering fields stays decodable as long as
    // the named entries match. Locked in by the
    // `unknown_field_is_forward_compatible` test below.
    rmp_serde::to_vec_named(v).map_err(|e| {
        WaferError::new(
            ErrorCode::Internal,
            format!("codec encode error in {}: {e}", std::any::type_name::<T>()),
        )
    })
}

/// Decode MessagePack bytes into `T`. Unknown fields are ignored (forward
/// compatibility). Nesting deeper than [`WIRE_MAX_DEPTH`] is rejected before
/// the recursive decode can endanger the stack. Returns a [`WaferError`]
/// with [`ErrorCode::Internal`] on deserialization failure.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, WaferError> {
    let mut de = rmp_serde::Deserializer::from_read_ref(bytes);
    de.set_max_depth(WIRE_MAX_DEPTH);
    T::deserialize(&mut de).map_err(|e| {
        WaferError::new(
            ErrorCode::Internal,
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

    /// `depth` nested single-element msgpack arrays terminated by nil —
    /// the smallest hostile deep body (one 0x91 byte per level).
    fn nested_array_bytes(depth: usize) -> Vec<u8> {
        let mut bytes = vec![0x91u8; depth];
        bytes.push(0xc0);
        bytes
    }

    #[test]
    fn decode_rejects_depth_beyond_wire_cap() {
        // Just above the cap: must be a clean error, not a decode.
        let bytes = nested_array_bytes(super::WIRE_MAX_DEPTH + 1);
        let result: Result<serde_json::Value, _> = decode(&bytes);
        let err = result.expect_err("body deeper than WIRE_MAX_DEPTH must be rejected");
        assert!(
            err.message.contains("depth"),
            "error should name the depth limit; got: {}",
            err.message
        );
    }

    #[test]
    fn decode_rejects_hostile_deep_body_without_crashing() {
        // Regression for the I-1 audit finding: a hostile ~1K-deep body
        // stack-overflowed (uncatchable abort) inside rmp-serde's recursive
        // decode BEFORE its default depth limit (1024) could fire — the
        // limit was deeper than the stack. With the explicit wire cap the
        // recursion stops long before any stack risk, natively and on
        // small-stack wasm32 targets.
        for depth in [1023, 50_000] {
            let bytes = nested_array_bytes(depth);
            let result: Result<serde_json::Value, _> = decode(&bytes);
            assert!(
                result.is_err(),
                "hostile {depth}-deep body must be rejected"
            );
        }
    }

    #[test]
    fn decode_accepts_realistic_nesting() {
        // Half the cap must decode fine — the cap exists for hostile
        // bodies, not to constrain real payloads (FilterNode is bounded at
        // depth 16 post-decode; user JSON values are modest).
        let bytes = nested_array_bytes(super::WIRE_MAX_DEPTH / 2);
        let result: Result<serde_json::Value, _> = decode(&bytes);
        assert!(result.is_ok(), "{:?}", result.unwrap_err());
    }
}
