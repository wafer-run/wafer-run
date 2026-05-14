//! SEC-003 regression tests — service handlers cross-validate the
//! caller-supplied `wrap.resource` meta against the actual resource named in
//! the decoded payload. Mismatch → PERMISSION_DENIED, even if the runtime's
//! own WRAP check passed (which it might, because the meta names a resource
//! the caller has a grant for, but the payload targets a different resource).

use std::collections::HashMap;

use wafer_block::{
    codec,
    common::ServiceOp,
    meta::{META_WRAP_ACCESS, META_WRAP_RESOURCE, META_WRAP_RESOURCE_TYPE},
    wire, ErrorCode, Message, WaferError,
};

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

mod storage_fakes {
    use async_trait::async_trait;
    use wafer_core::interfaces::storage::service::{
        FolderInfo, ListOptions, ObjectInfo, ObjectList, StorageError, StorageService,
    };

    /// Stub StorageService — every op succeeds with empty data.
    pub struct OkStorage;

    #[async_trait]
    impl StorageService for OkStorage {
        async fn put(
            &self,
            _folder: &str,
            _key: &str,
            _data: &[u8],
            _content_type: &str,
        ) -> Result<(), StorageError> {
            Ok(())
        }
        async fn get(
            &self,
            _folder: &str,
            key: &str,
        ) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
            Ok((
                vec![],
                ObjectInfo {
                    key: key.to_string(),
                    size: 0,
                    content_type: "application/octet-stream".to_string(),
                    last_modified: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
                },
            ))
        }
        async fn delete(&self, _folder: &str, _key: &str) -> Result<(), StorageError> {
            Ok(())
        }
        async fn list(
            &self,
            _folder: &str,
            _opts: &ListOptions,
        ) -> Result<ObjectList, StorageError> {
            Ok(ObjectList {
                objects: vec![],
                total_count: 0,
            })
        }
        async fn create_folder(&self, _name: &str, _public: bool) -> Result<(), StorageError> {
            Ok(())
        }
        async fn delete_folder(&self, _name: &str) -> Result<(), StorageError> {
            Ok(())
        }
        async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
            Ok(vec![])
        }
    }
}

fn msg_with_meta(kind: &str, resource: &str, access: &str, rt: &str) -> Message {
    let mut m = Message::new(kind);
    m.set_meta(META_WRAP_RESOURCE, resource);
    m.set_meta(META_WRAP_ACCESS, access);
    m.set_meta(META_WRAP_RESOURCE_TYPE, rt);
    m
}

async fn terminal_error(out: wafer_block::streams::output::OutputStream) -> Option<WaferError> {
    match out.collect_buffered().await {
        Ok(_) => None,
        Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => Some(e),
        _ => None,
    }
}

#[tokio::test]
async fn storage_put_rejects_mismatched_resource() {
    let svc = storage_fakes::OkStorage;
    let req = wire::storage::PutRequest {
        folder: "uploads".into(),
        key: "real.png".into(),
        data: vec![],
        content_type: "image/png".into(),
    };
    let body = codec::encode(&req).unwrap();
    // Meta claims a different key — handler must reject.
    let msg = msg_with_meta(
        ServiceOp::STORAGE_PUT,
        "uploads/decoy.png",
        "write",
        "storage",
    );
    let out = wafer_core::interfaces::storage::handler::handle_message(&svc, &msg, &body).await;
    let err = terminal_error(out).await.expect("expected error");
    assert_eq!(err.code, ErrorCode::PERMISSION_DENIED);
}

#[tokio::test]
async fn storage_put_accepts_matched_resource() {
    let svc = storage_fakes::OkStorage;
    let req = wire::storage::PutRequest {
        folder: "uploads".into(),
        key: "img.png".into(),
        data: vec![],
        content_type: "image/png".into(),
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_with_meta(
        ServiceOp::STORAGE_PUT,
        "uploads/img.png",
        "write",
        "storage",
    );
    let out = wafer_core::interfaces::storage::handler::handle_message(&svc, &msg, &body).await;
    assert!(terminal_error(out).await.is_none());
}

#[tokio::test]
async fn storage_delete_rejects_mismatched_resource() {
    let svc = storage_fakes::OkStorage;
    let req = wire::storage::DeleteRequest {
        folder: "uploads".into(),
        key: "real.png".into(),
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_with_meta(
        ServiceOp::STORAGE_DELETE,
        "uploads/decoy.png",
        "write",
        "storage",
    );
    let out = wafer_core::interfaces::storage::handler::handle_message(&svc, &msg, &body).await;
    let err = terminal_error(out).await.expect("expected error");
    assert_eq!(err.code, ErrorCode::PERMISSION_DENIED);
}

#[tokio::test]
async fn storage_list_rejects_mismatched_folder() {
    let svc = storage_fakes::OkStorage;
    let req = wire::storage::ListRequest {
        folder: "uploads".into(),
        prefix: String::new(),
        limit: 0,
        offset: 0,
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_with_meta(ServiceOp::STORAGE_LIST, "decoy", "read", "storage");
    let out = wafer_core::interfaces::storage::handler::handle_message(&svc, &msg, &body).await;
    let err = terminal_error(out).await.expect("expected error");
    assert_eq!(err.code, ErrorCode::PERMISSION_DENIED);
}

#[tokio::test]
async fn storage_create_folder_rejects_mismatched_name() {
    let svc = storage_fakes::OkStorage;
    let req = wire::storage::CreateFolderRequest {
        name: "real".into(),
        public: false,
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_with_meta(
        ServiceOp::STORAGE_CREATE_FOLDER,
        "decoy",
        "write",
        "storage",
    );
    let out = wafer_core::interfaces::storage::handler::handle_message(&svc, &msg, &body).await;
    let err = terminal_error(out).await.expect("expected error");
    assert_eq!(err.code, ErrorCode::PERMISSION_DENIED);
}

// ---------------------------------------------------------------------------
// Crypto
// ---------------------------------------------------------------------------

mod crypto_fakes {
    use std::{collections::HashMap, time::Duration};

    use wafer_core::interfaces::crypto::service::{CryptoError, CryptoService};

    pub struct OkCrypto;

    impl CryptoService for OkCrypto {
        fn hash(&self, _password: &str) -> Result<String, CryptoError> {
            Ok("hash".into())
        }
        fn compare_hash(&self, _password: &str, _hash: &str) -> Result<(), CryptoError> {
            Ok(())
        }
        fn sign(
            &self,
            _claims: HashMap<String, serde_json::Value>,
            _expiry: Duration,
        ) -> Result<String, CryptoError> {
            Ok("token".into())
        }
        fn verify(&self, _token: &str) -> Result<HashMap<String, serde_json::Value>, CryptoError> {
            Ok(HashMap::new())
        }
        fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError> {
            Ok(vec![0; n])
        }
    }
}

#[tokio::test]
async fn crypto_sign_rejects_mismatched_op() {
    let svc = crypto_fakes::OkCrypto;
    let req = wire::crypto::SignRequest {
        claims: HashMap::new(),
        expiry_secs: 60,
    };
    let body = codec::encode(&req).unwrap();
    // Caller meta claims `random_bytes` but actually signs — must reject.
    let msg = msg_with_meta(ServiceOp::CRYPTO_SIGN, "random_bytes", "read", "crypto");
    let out = wafer_core::interfaces::crypto::handler::handle_message(&svc, None, &msg, &body);
    let err = terminal_error(out).await.expect("expected error");
    assert_eq!(err.code, ErrorCode::PERMISSION_DENIED);
}

#[tokio::test]
async fn crypto_sign_accepts_matched_op() {
    let svc = crypto_fakes::OkCrypto;
    let req = wire::crypto::SignRequest {
        claims: HashMap::new(),
        expiry_secs: 60,
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_with_meta(ServiceOp::CRYPTO_SIGN, "sign", "read", "crypto");
    let out = wafer_core::interfaces::crypto::handler::handle_message(&svc, None, &msg, &body);
    assert!(terminal_error(out).await.is_none());
}

#[tokio::test]
async fn crypto_random_bytes_rejects_mismatched_op() {
    let svc = crypto_fakes::OkCrypto;
    let req = wire::crypto::RandomBytesRequest { n: 8 };
    let body = codec::encode(&req).unwrap();
    let msg = msg_with_meta(ServiceOp::CRYPTO_RANDOM_BYTES, "sign", "read", "crypto");
    let out = wafer_core::interfaces::crypto::handler::handle_message(&svc, None, &msg, &body);
    let err = terminal_error(out).await.expect("expected error");
    assert_eq!(err.code, ErrorCode::PERMISSION_DENIED);
}

// ---------------------------------------------------------------------------
// Database
// ---------------------------------------------------------------------------

mod db_fakes {
    use async_trait::async_trait;
    use wafer_core::interfaces::database::service::{
        DatabaseError, DatabaseService, Filter, ListOptions, Record, RecordList,
    };
    use wafer_run::schema::{Column, Table};

    pub struct OkDb;

    #[async_trait]
    impl DatabaseService for OkDb {
        async fn get(&self, _collection: &str, id: &str) -> Result<Record, DatabaseError> {
            Ok(Record {
                id: id.to_string(),
                data: Default::default(),
            })
        }
        async fn list(
            &self,
            _collection: &str,
            _opts: &ListOptions,
        ) -> Result<RecordList, DatabaseError> {
            Ok(RecordList {
                records: vec![],
                total_count: 0,
                page: 1,
                page_size: 0,
            })
        }
        async fn create(
            &self,
            _collection: &str,
            data: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<Record, DatabaseError> {
            Ok(Record {
                id: "new".into(),
                data,
            })
        }
        async fn update(
            &self,
            _collection: &str,
            id: &str,
            data: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<Record, DatabaseError> {
            Ok(Record {
                id: id.to_string(),
                data,
            })
        }
        async fn delete(&self, _collection: &str, _id: &str) -> Result<(), DatabaseError> {
            Ok(())
        }
        async fn count(
            &self,
            _collection: &str,
            _filters: &[Filter],
        ) -> Result<i64, DatabaseError> {
            Ok(0)
        }
        async fn sum(
            &self,
            _collection: &str,
            _field: &str,
            _filters: &[Filter],
        ) -> Result<f64, DatabaseError> {
            Ok(0.0)
        }
        async fn query_raw(
            &self,
            _query: &str,
            _args: &[serde_json::Value],
        ) -> Result<Vec<Record>, DatabaseError> {
            Ok(vec![])
        }
        async fn exec_raw(
            &self,
            _query: &str,
            _args: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            Ok(0)
        }
        async fn delete_where(
            &self,
            _collection: &str,
            _filters: &[Filter],
        ) -> Result<(), DatabaseError> {
            Ok(())
        }
        async fn delete_where_count(
            &self,
            _collection: &str,
            _filters: &[Filter],
        ) -> Result<i64, DatabaseError> {
            Ok(0)
        }
        async fn take_where(
            &self,
            _collection: &str,
            _filters: &[Filter],
        ) -> Result<Vec<Record>, DatabaseError> {
            Ok(vec![])
        }
        async fn update_where(
            &self,
            _collection: &str,
            _filters: &[Filter],
            _data: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<(), DatabaseError> {
            Ok(())
        }
        async fn ensure_schema_table(&self, _table: &Table) -> Result<(), DatabaseError> {
            Ok(())
        }
        async fn schema_table_exists(&self, _name: &str) -> Result<bool, DatabaseError> {
            Ok(true)
        }
        async fn schema_drop_table(&self, _name: &str) -> Result<(), DatabaseError> {
            Ok(())
        }
        async fn schema_add_column(
            &self,
            _table: &str,
            _column: &Column,
        ) -> Result<(), DatabaseError> {
            Ok(())
        }
    }
}

#[tokio::test]
async fn database_get_rejects_mismatched_collection() {
    let svc = db_fakes::OkDb;
    let req = wire::database::GetRequest {
        collection: "real_table".into(),
        id: "1".into(),
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_with_meta(ServiceOp::DATABASE_GET, "decoy_table", "read", "db");
    let out = wafer_core::interfaces::database::handler::handle_message(&svc, &msg, &body).await;
    let err = terminal_error(out).await.expect("expected error");
    assert_eq!(err.code, ErrorCode::PERMISSION_DENIED);
}

#[tokio::test]
async fn database_create_rejects_mismatched_collection() {
    let svc = db_fakes::OkDb;
    let req = wire::database::CreateRequest {
        collection: "real_table".into(),
        data: Default::default(),
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_with_meta(ServiceOp::DATABASE_CREATE, "decoy", "write", "db");
    let out = wafer_core::interfaces::database::handler::handle_message(&svc, &msg, &body).await;
    let err = terminal_error(out).await.expect("expected error");
    assert_eq!(err.code, ErrorCode::PERMISSION_DENIED);
}

#[tokio::test]
async fn database_update_where_rejects_mismatched_collection() {
    let svc = db_fakes::OkDb;
    let req = wire::database::UpdateWhereRequest {
        collection: "real_table".into(),
        filters: vec![],
        data: Default::default(),
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_with_meta(ServiceOp::DATABASE_UPDATE_WHERE, "decoy", "write", "db");
    let out = wafer_core::interfaces::database::handler::handle_message(&svc, &msg, &body).await;
    let err = terminal_error(out).await.expect("expected error");
    assert_eq!(err.code, ErrorCode::PERMISSION_DENIED);
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

mod config_fakes {
    use wafer_core::interfaces::config::service::ConfigService;

    pub struct OkConfig;

    impl ConfigService for OkConfig {
        fn get(&self, _key: &str) -> Option<String> {
            Some("value".into())
        }
        fn set(&self, _key: &str, _value: &str) {}
    }
}

#[tokio::test]
async fn config_get_rejects_mismatched_key() {
    let svc = config_fakes::OkConfig;
    let req = wire::config::GetRequest {
        key: "REAL_KEY".into(),
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_with_meta(ServiceOp::CONFIG_GET, "DECOY_KEY", "read", "config");
    let out = wafer_core::interfaces::config::handler::handle_message(&svc, &msg, &body);
    let err = terminal_error(out).await.expect("expected error");
    assert_eq!(err.code, ErrorCode::PERMISSION_DENIED);
}

#[tokio::test]
async fn config_set_rejects_mismatched_key() {
    let svc = config_fakes::OkConfig;
    let req = wire::config::SetRequest {
        key: "REAL_KEY".into(),
        value: "v".into(),
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_with_meta(ServiceOp::CONFIG_SET, "DECOY_KEY", "write", "config");
    let out = wafer_core::interfaces::config::handler::handle_message(&svc, &msg, &body);
    let err = terminal_error(out).await.expect("expected error");
    assert_eq!(err.code, ErrorCode::PERMISSION_DENIED);
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

mod network_fakes {
    use async_trait::async_trait;
    use wafer_core::interfaces::network::service::{
        NetworkError, NetworkService, Request, Response,
    };

    pub struct OkNetwork;

    #[async_trait]
    impl NetworkService for OkNetwork {
        async fn do_request(&self, _req: &Request) -> Result<Response, NetworkError> {
            Ok(Response {
                status_code: 200,
                headers: Default::default(),
                body: vec![],
            })
        }
    }
}

#[tokio::test]
async fn network_do_rejects_mismatched_url() {
    let svc = network_fakes::OkNetwork;
    let req = wire::network::Request {
        method: "GET".into(),
        url: "https://real.example.com/data".into(),
        headers: HashMap::new(),
        body: None,
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_with_meta(
        ServiceOp::NETWORK_DO_REQUEST,
        "https://decoy.example.com/data",
        "read",
        "network",
    );
    let out = wafer_core::interfaces::network::handler::handle_message(&svc, &msg, &body).await;
    let err = terminal_error(out).await.expect("expected error");
    assert_eq!(err.code, ErrorCode::PERMISSION_DENIED);
}
