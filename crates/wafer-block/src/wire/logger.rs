//! Wire-format types for the logger service.
//!
//! Mirrors `crates/wafer-core/src/interfaces/logger/handler.rs`. All four
//! verbs (`debug`, `info`, `warn`, `error`) share a single `LogRequest`
//! payload — only `Message::kind` distinguishes the level. Logger calls are
//! fire-and-forget; there is no response payload.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRequest {
    #[serde(default)]
    pub message: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub fields: HashMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    #[test]
    fn log_request_round_trips() {
        let mut fields = HashMap::new();
        fields.insert("user_id".into(), serde_json::json!("u1"));
        fields.insert("retries".into(), serde_json::json!(3));
        let original = LogRequest {
            message: "request handled".into(),
            fields,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: LogRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.message, original.message);
        assert_eq!(
            decoded.fields.get("user_id"),
            Some(&serde_json::json!("u1"))
        );
        assert_eq!(decoded.fields.get("retries"), Some(&serde_json::json!(3)));
    }

    #[test]
    fn schema_lock_log_request() {
        let req = LogRequest {
            message: String::new(),
            fields: HashMap::new(),
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a76d657373616765a0",
            "LogRequest schema changed — review consumer impact before updating this literal"
        );
    }
}
