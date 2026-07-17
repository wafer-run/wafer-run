//! Wire-format types for the storage service.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// --- Requests ---

/// Request for `storage.put`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutRequest {
    /// Folder (bucket) name.
    pub folder: String,
    /// Object key within `folder`.
    pub key: String,
    /// Object body bytes.
    pub data: Vec<u8>,
    /// MIME type (defaults to `application/octet-stream`).
    #[serde(default = "default_content_type")]
    pub content_type: String,
}

fn default_content_type() -> String {
    "application/octet-stream".to_string()
}

/// Header frame for `storage.put_streaming` — the object metadata that
/// precedes the streamed object body on the wire.
///
/// The streaming-upload protocol frames the request as this header (frame 1)
/// followed by zero-or-more raw body-chunk frames, so the object bytes never
/// sit in a single buffered `data: Vec<u8>` field the way [`PutRequest`]
/// encodes them. Field-for-field this is [`PutRequest`] minus `data`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutStreamingHeader {
    /// Folder (bucket) name.
    pub folder: String,
    /// Object key within `folder`.
    pub key: String,
    /// MIME type (defaults to `application/octet-stream`).
    #[serde(default = "default_content_type")]
    pub content_type: String,
}

/// Request for `storage.get`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetRequest {
    /// Folder (bucket) name.
    pub folder: String,
    /// Object key within `folder`.
    pub key: String,
}

/// Request for `storage.delete`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteRequest {
    /// Folder (bucket) name.
    pub folder: String,
    /// Object key within `folder`.
    pub key: String,
}

/// Request for `storage.list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListRequest {
    /// Folder (bucket) name.
    pub folder: String,
    /// Optional key prefix to filter on.
    #[serde(default)]
    pub prefix: String,
    /// Maximum number of objects to return (0 = backend default).
    #[serde(default)]
    pub limit: i64,
    /// Number of objects to skip for pagination. Ignored when `cursor` is set.
    #[serde(default)]
    pub offset: i64,
    /// Opaque continuation token for cursor-based pagination. When present the
    /// backend returns the page after this token and ignores `offset`; when
    /// absent (the default) the query is offset-based. Omitted from the wire
    /// entirely when `None`, so an older peer that never sends it still
    /// decodes and offset callers produce byte-identical requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Request for `storage.create_folder`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateFolderRequest {
    /// Folder name to create.
    pub name: String,
    /// Whether the folder grants public read access.
    #[serde(default)]
    pub public: bool,
}

/// Request for `storage.delete_folder`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteFolderRequest {
    /// Folder name to delete.
    pub name: String,
}

// --- Responses ---

/// Metadata about a stored object, returned inside `GetResponse` and `ObjectList`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectInfo {
    /// Object key.
    pub key: String,
    /// Object size in bytes.
    pub size: i64,
    /// MIME type.
    pub content_type: String,
    /// Last-modified timestamp (UTC).
    pub last_modified: DateTime<Utc>,
}

/// Response for `storage.get`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetResponse {
    /// Object body bytes.
    pub data: Vec<u8>,
    /// Object metadata.
    pub info: ObjectInfo,
}

/// Paginated list of objects returned by the `list` operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectList {
    /// Objects in this page.
    pub objects: Vec<ObjectInfo>,
    /// Total number of objects matching the filter (across all pages).
    ///
    /// Backends where an exact total would require walking the entire
    /// keyspace (e.g. S3) may return a lower bound when a `limit` was
    /// given and the listing stopped early; the bound stays strictly
    /// greater than `offset + limit` while more objects exist. In cursor
    /// mode it is only the current page's object count — a lower bound;
    /// cursor callers use `next_cursor` for the has-more signal.
    pub total_count: i64,
    /// Opaque, backend-defined continuation token for the next page, or
    /// `None` when there are no more objects. Callers MUST treat it as opaque
    /// (never parse it) and only ever feed it back to the same backend via
    /// [`ListRequest::cursor`]. Omitted from the wire when `None`, so an older
    /// peer that never sends it still decodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Metadata about a storage folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderInfo {
    /// Folder name.
    pub name: String,
    /// Whether the folder grants public read access.
    pub public: bool,
    /// Folder creation timestamp (UTC).
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
    fn put_streaming_header_round_trips() {
        let original = PutStreamingHeader {
            folder: "uploads".into(),
            key: "avatar.png".into(),
            content_type: "image/png".into(),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: PutStreamingHeader = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.folder, original.folder);
        assert_eq!(decoded.key, original.key);
        assert_eq!(decoded.content_type, original.content_type);
    }

    #[test]
    fn put_streaming_header_defaults_content_type() {
        // A header encoded without `content_type` (e.g. an older/leaner
        // producer) must still decode, defaulting the MIME type — mirrors
        // `PutRequest`'s `#[serde(default)]` behavior.
        #[derive(serde::Serialize)]
        struct LeanHeader {
            folder: String,
            key: String,
        }
        let encoded = codec::encode(&LeanHeader {
            folder: "f".into(),
            key: "k".into(),
        })
        .expect("encode");
        let decoded: PutStreamingHeader = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.content_type, "application/octet-stream");
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
            cursor: None,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ListRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.folder, original.folder);
        assert_eq!(decoded.prefix, original.prefix);
        assert_eq!(decoded.limit, original.limit);
        assert_eq!(decoded.offset, original.offset);
        assert_eq!(decoded.cursor, original.cursor);
    }

    #[test]
    fn list_request_round_trips_with_cursor() {
        let original = ListRequest {
            folder: "uploads".into(),
            prefix: String::new(),
            limit: 50,
            offset: 0,
            cursor: Some("opaque-continuation-token".into()),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ListRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.cursor.as_deref(), Some("opaque-continuation-token"));
    }

    #[test]
    fn list_request_cursor_omitted_when_none_and_defaults_on_decode() {
        // An offset-only request must encode byte-identically to the pre-cursor
        // wire (the `cursor` key is skipped when `None`), and a request encoded
        // by an older peer that has no `cursor` field must still decode,
        // defaulting the cursor to `None`.
        #[derive(serde::Serialize)]
        struct LegacyListRequest {
            folder: String,
            prefix: String,
            limit: i64,
            offset: i64,
        }
        let legacy = LegacyListRequest {
            folder: "uploads".into(),
            prefix: "2024/".into(),
            limit: 50,
            offset: 100,
        };
        let current = ListRequest {
            folder: "uploads".into(),
            prefix: "2024/".into(),
            limit: 50,
            offset: 100,
            cursor: None,
        };
        assert_eq!(
            codec::encode(&current).expect("encode current"),
            codec::encode(&legacy).expect("encode legacy"),
            "an offset-only ListRequest must be byte-identical to the pre-cursor wire"
        );
        let decoded: ListRequest =
            codec::decode(&codec::encode(&legacy).expect("encode")).expect("decode");
        assert!(decoded.cursor.is_none());
    }

    #[test]
    fn object_list_round_trips_with_next_cursor() {
        let original = ObjectList {
            objects: vec![ObjectInfo {
                key: "a".into(),
                size: 1,
                content_type: "text/plain".into(),
                last_modified: DateTime::from_timestamp(0, 0).unwrap(),
            }],
            total_count: 1,
            next_cursor: Some("next-page-token".into()),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ObjectList = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.objects.len(), 1);
        assert_eq!(decoded.total_count, 1);
        assert_eq!(decoded.next_cursor.as_deref(), Some("next-page-token"));
    }

    #[test]
    fn object_list_next_cursor_omitted_when_none_and_defaults_on_decode() {
        // A final-page response (no `next_cursor`) must be byte-identical to
        // the pre-cursor wire, and a response encoded by an older peer that
        // has no `next_cursor` field must still decode, defaulting to `None`.
        #[derive(serde::Serialize)]
        struct LegacyObjectList {
            objects: Vec<ObjectInfo>,
            total_count: i64,
        }
        let legacy = LegacyObjectList {
            objects: vec![],
            total_count: 3,
        };
        let current = ObjectList {
            objects: vec![],
            total_count: 3,
            next_cursor: None,
        };
        assert_eq!(
            codec::encode(&current).expect("encode current"),
            codec::encode(&legacy).expect("encode legacy"),
            "a final-page ObjectList must be byte-identical to the pre-cursor wire"
        );
        let decoded: ObjectList =
            codec::decode(&codec::encode(&legacy).expect("encode")).expect("decode");
        assert!(decoded.next_cursor.is_none());
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
    fn schema_lock_put_streaming_header() {
        let header = PutStreamingHeader {
            folder: String::new(),
            key: String::new(),
            content_type: String::new(),
        };
        let encoded = codec::encode(&header).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "83a6666f6c646572a0a36b6579a0ac636f6e74656e745f74797065a0",
            "PutStreamingHeader schema changed — review consumer impact before updating this literal"
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
