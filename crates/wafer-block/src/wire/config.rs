//! Wire-format types for the config service.
//!
//! Mirrors `crates/wafer-core/src/interfaces/config/handler.rs`.

use serde::{Deserialize, Serialize};

// --- Requests ---

/// Request for `config.get`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetRequest {
    /// Full config key (e.g. `MY_ORG__JWT_SECRET`).
    pub key: String,
}

/// Request for `config.set`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetRequest {
    /// Full config key to write.
    pub key: String,
    /// New stringified value.
    pub value: String,
}

// --- Responses ---

/// Response for `config.get`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetResponse {
    /// Resolved value, or `""` when the key is unset.
    pub value: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    #[test]
    fn get_request_round_trips() {
        let original = GetRequest {
            key: "DATABASE_URL".into(),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: GetRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.key, original.key);
    }

    #[test]
    fn set_request_round_trips() {
        let original = SetRequest {
            key: "FOO".into(),
            value: "bar".into(),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: SetRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.key, original.key);
        assert_eq!(decoded.value, original.value);
    }

    #[test]
    fn get_response_round_trips() {
        let original = GetResponse { value: "v".into() };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: GetResponse = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.value, original.value);
    }

    #[test]
    fn schema_lock_get_request() {
        let req = GetRequest { key: String::new() };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a36b6579a0",
            "GetRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_set_request() {
        let req = SetRequest {
            key: String::new(),
            value: String::new(),
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82a36b6579a0a576616c7565a0",
            "SetRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_get_response() {
        let resp = GetResponse {
            value: String::new(),
        };
        let encoded = codec::encode(&resp).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a576616c7565a0",
            "GetResponse schema changed — review consumer impact before updating this literal"
        );
    }
}
