//! Wire-format types for the network service.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Outbound HTTP request payload for `network.do`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// HTTP method (e.g. `"GET"`, `"POST"`).
    pub method: String,
    /// Target URL.
    pub url: String,
    /// Request headers (single value per name).
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Optional request body bytes.
    #[serde(default)]
    pub body: Option<Vec<u8>>,
}

/// Header frame — emitted as the first chunk on the streaming response.
/// Contains all the response metadata except the body itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseHeader {
    /// HTTP status code (e.g. `200`, `404`).
    pub status_code: u16,
    /// Response headers (each name may have multiple values).
    pub headers: HashMap<String, Vec<String>>,
}

/// Buffered convenience type — header fields plus the full body. The buffered
/// SDK helper reconstructs this from `ResponseHeader` + accumulated chunks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// HTTP status code (e.g. `200`, `404`).
    pub status_code: u16,
    /// Response headers (each name may have multiple values).
    pub headers: HashMap<String, Vec<String>>,
    /// Full response body bytes.
    pub body: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    #[test]
    fn request_round_trips() {
        let original = Request {
            method: "POST".into(),
            url: "https://example.test/upload".into(),
            headers: [("X-Test".to_string(), "1".to_string())]
                .into_iter()
                .collect(),
            body: Some(vec![1, 2, 3, 4, 5]),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: Request = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.method, original.method);
        assert_eq!(decoded.url, original.url);
        assert_eq!(decoded.body, original.body);
        assert_eq!(decoded.headers.get("X-Test"), Some(&"1".to_string()));
    }

    #[test]
    fn response_header_round_trips() {
        let original = ResponseHeader {
            status_code: 200,
            headers: [("content-type".to_string(), vec!["image/png".to_string()])]
                .into_iter()
                .collect(),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ResponseHeader = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.status_code, 200);
        assert_eq!(
            decoded.headers.get("content-type"),
            Some(&vec!["image/png".to_string()])
        );
    }

    #[test]
    fn response_body_no_inflation() {
        // Locks the load-bearing invariant for the network service specifically.
        let response = Response {
            status_code: 200,
            headers: HashMap::new(),
            body: vec![0u8; 1024 * 1024],
        };
        let encoded = codec::encode(&response).expect("encode");
        assert!(
            encoded.len() < 1024 * 1024 + (1024 * 1024 / 20),
            "Response body inflated to {} bytes — should be ~1 MiB",
            encoded.len()
        );
    }

    #[test]
    fn schema_lock_request() {
        // Catches accidental wire-incompatible changes (renamed/reordered fields).
        // If this test fails because of an intentional schema update, update the
        // hex literal — but think about consumer impact first.
        let req = Request {
            method: "GET".into(),
            url: "x".into(),
            headers: HashMap::new(),
            body: None,
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        // The exact hex is a schema lock. Update only when the schema
        // intentionally changes; review consumer impact first.
        assert_eq!(
            hex, "84a66d6574686f64a3474554a375726ca178a76865616465727380a4626f6479c0",
            "Request schema changed — review consumer impact before updating this literal"
        );
    }
}
