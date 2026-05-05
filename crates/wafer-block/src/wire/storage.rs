//! Wire-format types for the storage service.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// --- Requests ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutRequest {
    pub folder: String,
    pub key: String,
    pub data: Vec<u8>,
    #[serde(default = "default_content_type")]
    pub content_type: String,
}

fn default_content_type() -> String {
    "application/octet-stream".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetRequest {
    pub folder: String,
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteRequest {
    pub folder: String,
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListRequest {
    pub folder: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateFolderRequest {
    pub name: String,
    #[serde(default)]
    pub public: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteFolderRequest {
    pub name: String,
}

// --- Responses ---

/// Metadata about a stored object, returned inside `GetResponse` and `ObjectList`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectInfo {
    pub key: String,
    pub size: i64,
    pub content_type: String,
    pub last_modified: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetResponse {
    pub data: Vec<u8>,
    pub info: ObjectInfo,
}

/// Paginated list of objects returned by the `list` operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectList {
    pub objects: Vec<ObjectInfo>,
    pub total_count: i64,
}

/// Metadata about a storage folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderInfo {
    pub name: String,
    pub public: bool,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    // -----------------------------------------------------------------------
    // Round-trip tests
    // -----------------------------------------------------------------------

    #[test]
    fn put_request_round_trips() {
        let original = PutRequest {
            folder: "uploads".into(),
            key: "avatar.png".into(),
            data: vec![0xDE, 0xAD, 0xBE, 0xEF],
            content_type: "image/png".into(),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: PutRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.folder, original.folder);
        assert_eq!(decoded.key, original.key);
        assert_eq!(decoded.data, original.data);
        assert_eq!(decoded.content_type, original.content_type);
    }

    #[test]
    fn get_response_round_trips() {
        let original = GetResponse {
            data: vec![1, 2, 3],
            info: ObjectInfo {
                key: "avatar.png".into(),
                size: 3,
                content_type: "image/png".into(),
                last_modified: DateTime::from_timestamp(0, 0).unwrap(),
            },
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: GetResponse = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.data, original.data);
        assert_eq!(decoded.info.key, original.info.key);
        assert_eq!(decoded.info.size, original.info.size);
        assert_eq!(decoded.info.content_type, original.info.content_type);
    }

    #[test]
    fn list_request_round_trips() {
        let original = ListRequest {
            folder: "uploads".into(),
            prefix: "2024/".into(),
            limit: 50,
            offset: 100,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ListRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.folder, original.folder);
        assert_eq!(decoded.prefix, original.prefix);
        assert_eq!(decoded.limit, original.limit);
        assert_eq!(decoded.offset, original.offset);
    }

    // -----------------------------------------------------------------------
    // No-inflation tests (Vec<u8> fields must not balloon encoded size)
    // -----------------------------------------------------------------------

    #[test]
    fn put_request_no_inflation() {
        // 1 MiB body must fit in ~1.05 MiB encoded (5% headroom).
        let req = PutRequest {
            folder: "f".into(),
            key: "k".into(),
            data: vec![0u8; 1024 * 1024],
            content_type: "application/octet-stream".into(),
        };
        let encoded = codec::encode(&req).expect("encode");
        assert!(
            encoded.len() < 1024 * 1024 + (1024 * 1024 / 20),
            "PutRequest data inflated to {} bytes — should be ~1 MiB",
            encoded.len()
        );
    }

    #[test]
    fn get_response_no_inflation() {
        // 1 MiB body must fit in ~1.05 MiB encoded (5% headroom).
        let resp = GetResponse {
            data: vec![0u8; 1024 * 1024],
            info: ObjectInfo {
                key: "k".into(),
                size: 1024 * 1024,
                content_type: "application/octet-stream".into(),
                last_modified: DateTime::from_timestamp(0, 0).unwrap(),
            },
        };
        let encoded = codec::encode(&resp).expect("encode");
        assert!(
            encoded.len() < 1024 * 1024 + (1024 * 1024 / 20),
            "GetResponse data inflated to {} bytes — should be ~1 MiB",
            encoded.len()
        );
    }

    // -----------------------------------------------------------------------
    // Schema-lock tests (catch accidental wire-incompatible field renames)
    // -----------------------------------------------------------------------

    #[test]
    fn schema_lock_put_request() {
        let req = PutRequest {
            folder: String::new(),
            key: String::new(),
            data: vec![],
            content_type: String::new(),
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "84a6666f6c646572a0a36b6579a0a46461746190ac636f6e74656e745f74797065a0",
            "PutRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_get_request() {
        let req = GetRequest {
            folder: String::new(),
            key: String::new(),
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82a6666f6c646572a0a36b6579a0",
            "GetRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_get_response() {
        let resp = GetResponse {
            data: vec![],
            info: ObjectInfo {
                key: String::new(),
                size: 0,
                content_type: String::new(),
                last_modified: DateTime::from_timestamp(0, 0).unwrap(),
            },
        };
        let encoded = codec::encode(&resp).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "82a46461746190a4696e666f84a36b6579a0a473697a6500ac636f6e74656e745f74797065a0ad6c6173745f6d6f646966696564b4313937302d30312d30315430303a30303a30305a",
            "GetResponse schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_delete_request() {
        let req = DeleteRequest {
            folder: String::new(),
            key: String::new(),
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82a6666f6c646572a0a36b6579a0",
            "DeleteRequest schema changed — review consumer impact before updating this literal"
        );
    }
}
