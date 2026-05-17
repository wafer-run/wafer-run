//! Wire-format types for the crypto service.
//!
//! Mirrors `crates/wafer-core/src/interfaces/crypto/handler.rs`. Includes
//! `Vec<u8>` body fields (`RandomBytesResponse::bytes`) — no-inflation tests
//! lock the load-bearing invariant.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// --- Requests ---

/// Request for `crypto.hash` — produce a salted hash of `password`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HashRequest {
    /// Plaintext password to hash.
    pub password: String,
}

/// Request for `crypto.compare_hash` — verify a candidate against a stored
/// hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompareHashRequest {
    /// Candidate plaintext password.
    pub password: String,
    /// Previously produced hash string.
    pub hash: String,
}

/// Request for `crypto.sign` — produce a signed token over `claims`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignRequest {
    /// Claims to encode into the token payload.
    pub claims: HashMap<String, serde_json::Value>,
    /// Lifetime of the token in seconds (defaults to 1h).
    #[serde(default = "default_expiry")]
    pub expiry_secs: u64,
}

fn default_expiry() -> u64 {
    3600
}

/// Request for `crypto.verify` — verify a token and return its claims.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyRequest {
    /// Token string to verify.
    pub token: String,
}

/// Request for `crypto.random_bytes` — request `n` cryptographically random
/// bytes (defaults to 32).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RandomBytesRequest {
    /// Number of bytes to generate.
    #[serde(default = "default_random_len")]
    pub n: usize,
}

fn default_random_len() -> usize {
    32
}

// --- Responses ---

/// Response for `crypto.hash`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HashResponse {
    /// Encoded hash string (algorithm-specific format).
    pub hash: String,
}

/// Response for `crypto.compare_hash`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompareHashResponse {
    /// Whether the candidate matched the hash. Serialized as `"match"` on
    /// the wire.
    #[serde(rename = "match")]
    pub matches: bool,
}

/// Response for `crypto.sign`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignResponse {
    /// Signed token string (e.g. JWT).
    pub token: String,
}

/// Response for `crypto.verify`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyResponse {
    /// Verified claims extracted from the token.
    pub claims: HashMap<String, serde_json::Value>,
}

/// Response for `crypto.random_bytes`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RandomBytesResponse {
    /// Random bytes (length = requested `n`).
    pub bytes: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    // -----------------------------------------------------------------------
    // Round-trip tests
    // -----------------------------------------------------------------------

    #[test]
    fn hash_request_round_trips() {
        let original = HashRequest {
            password: "hunter2".into(),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: HashRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.password, original.password);
    }

    #[test]
    fn compare_hash_request_round_trips() {
        let original = CompareHashRequest {
            password: "hunter2".into(),
            hash: "$argon2id$...".into(),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: CompareHashRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.password, original.password);
        assert_eq!(decoded.hash, original.hash);
    }

    #[test]
    fn sign_request_round_trips() {
        let mut claims = HashMap::new();
        claims.insert("sub".into(), serde_json::json!("user-1"));
        let original = SignRequest {
            claims,
            expiry_secs: 3600,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: SignRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.expiry_secs, 3600);
        assert_eq!(decoded.claims.get("sub"), original.claims.get("sub"));
    }

    #[test]
    fn verify_response_round_trips() {
        let mut claims = HashMap::new();
        claims.insert("sub".into(), serde_json::json!("u1"));
        let original = VerifyResponse { claims };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: VerifyResponse = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.claims.get("sub"), original.claims.get("sub"));
    }

    #[test]
    fn random_bytes_response_round_trips() {
        let original = RandomBytesResponse {
            bytes: vec![1, 2, 3, 4, 5],
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: RandomBytesResponse = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.bytes, original.bytes);
    }

    #[test]
    fn compare_hash_response_match_field_renamed() {
        // The wire field must be `match`, not `matches`. Round-trip a value
        // and re-decode through a struct that pulls `match` to confirm.
        let resp = CompareHashResponse { matches: true };
        let encoded = codec::encode(&resp).expect("encode");
        let value: serde_json::Value = rmp_serde::from_slice(&encoded).expect("decode to json");
        assert_eq!(value.get("match"), Some(&serde_json::Value::Bool(true)));
    }

    // -----------------------------------------------------------------------
    // No-inflation tests (Vec<u8> body fields must not balloon encoded size)
    // -----------------------------------------------------------------------

    #[test]
    fn random_bytes_response_no_inflation() {
        // 1 MiB body must fit in ~1.05 MiB encoded (5% headroom).
        let resp = RandomBytesResponse {
            bytes: vec![0u8; 1024 * 1024],
        };
        let encoded = codec::encode(&resp).expect("encode");
        assert!(
            encoded.len() < 1024 * 1024 + (1024 * 1024 / 20),
            "RandomBytesResponse bytes inflated to {} bytes — should be ~1 MiB",
            encoded.len()
        );
    }

    // -----------------------------------------------------------------------
    // Schema-lock tests
    // -----------------------------------------------------------------------

    #[test]
    fn schema_lock_hash_request() {
        let req = HashRequest {
            password: String::new(),
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a870617373776f7264a0",
            "HashRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_compare_hash_request() {
        let req = CompareHashRequest {
            password: String::new(),
            hash: String::new(),
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82a870617373776f7264a0a468617368a0",
            "CompareHashRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_sign_request() {
        let req = SignRequest {
            claims: HashMap::new(),
            expiry_secs: 0,
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82a6636c61696d7380ab6578706972795f7365637300",
            "SignRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_verify_request() {
        let req = VerifyRequest {
            token: String::new(),
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a5746f6b656ea0",
            "VerifyRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_random_bytes_request() {
        let req = RandomBytesRequest { n: 0 };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a16e00",
            "RandomBytesRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_random_bytes_response() {
        let resp = RandomBytesResponse { bytes: vec![] };
        let encoded = codec::encode(&resp).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a5627974657390",
            "RandomBytesResponse schema changed — review consumer impact before updating this literal"
        );
    }
}
