//! Task 7 — the COMPLETENESS GUARANTEE.
//!
//! Tasks 5-6 routed the database/storage/config/network/crypto handlers
//! through host-side WRAP authorization (`ctx.check_resource_access`, called
//! either via `decode_and_authorize` or, for `config.get`'s dual decode path,
//! directly). Those changes were verified op-by-op in
//! `handler_{database,storage,config,network,crypto}_wrap_authorization.rs`.
//!
//! This file adds the mechanical guarantee that makes the property durable:
//! it iterates the canonical op-family slices —
//! `ServiceOp::{DATABASE,STORAGE,CONFIG,NETWORK,CRYPTO}_OPS` — and, for every
//! op in each slice, dispatches a message with **no WRAP meta** (the exploit
//! shape) through the real handler under a `Context` whose
//! `check_resource_access` always denies. It asserts every one of them comes
//! back `PermissionDenied` *and* that the underlying service method never
//! ran (via a recording fake service per family).
//!
//! The mechanism that makes this "every op" instead of "every op we
//! remembered to write a test for": each family has a `*_op_body` function
//! that builds the minimal valid request body for a given op via an
//! exhaustive `match` over that family's op constants. If a future op is
//! added to a slice (e.g. `ServiceOp::DATABASE_OPS`) without a matching arm
//! here, the `match`'s fallback panics — failing this test immediately,
//! before anyone gets to ship the new op unauthorized. Combined with the
//! per-op dispatch loop, this is the structural guarantee: a new op cannot
//! silently skip authorization without this test either failing to compile
//! its body (missing arm → panic) or failing the deny assertion (missing
//! `check_resource_access` call in the handler arm).
//!
//! ## The one documented exception
//!
//! `ServiceOp::STORAGE_LIST_FOLDERS` has NO resource authorization in the
//! storage handler today — it lists every folder in the block's storage
//! backend with no per-folder grant check, because there is no "list-all"
//! WRAP sentinel yet (see `wafer-core/src/interfaces/storage/handler.rs`,
//! the `STORAGE_LIST_FOLDERS` arm calls `service.list_folders()` directly,
//! with no `decode_and_authorize` / `check_resource_access` call at all).
//! This is a tracked pre-existing residual, not a Task 7 regression — fixing
//! it needs a list-all sentinel design (analogous to `DDL_RESOURCE` /
//! `RAW_SQL_RESOURCE`) that is out of scope for the host-side-enforcement
//! task sequence — tracked as a dedicated follow-up (NOT Stage 2, which only
//! removes the dead meta-gated backstop).
//!
//! The completeness loop below explicitly skips it (with a citation to this
//! comment) rather than silently omitting it, and a separate test —
//! `storage_list_folders_residual_is_currently_unenforced` — pins its
//! *current* (unenforced) behavior: dispatching it under a denying `Context`
//! still reaches the service. If a future change adds enforcement, that
//! pinned test will fail, which is the intended signal to also promote
//! `STORAGE_LIST_FOLDERS` out of the skip-list above.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    Message, WaferError,
};

// ---------------------------------------------------------------------------
// Recording fakes — one per service family. Each records every op invoked so
// tests can assert a denied request never reached the service, not just that
// the handler returned the right error.
// ---------------------------------------------------------------------------

/// Shared call log, checked via `Arc::clone` from the test after the handler
/// call returns.
type Calls = Arc<Mutex<Vec<&'static str>>>;

fn new_calls() -> Calls {
    Arc::new(Mutex::new(Vec::new()))
}

mod db_fakes {
    use async_trait::async_trait;
    use wafer_block::db::{Filter, ListOptions};
    use wafer_core::interfaces::database::service::{
        DatabaseError, DatabaseService, Record, RecordList,
    };
    use wafer_schema::{Column, Table};

    use super::Calls;

    pub struct RecordingDb {
        pub calls: Calls,
    }

    impl RecordingDb {
        pub fn new(calls: Calls) -> Self {
            Self { calls }
        }

        fn record(&self, op: &'static str) {
            self.calls.lock().unwrap().push(op);
        }
    }

    #[async_trait]
    impl DatabaseService for RecordingDb {
        async fn get(&self, _collection: &str, id: &str) -> Result<Record, DatabaseError> {
            self.record("get");
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
            self.record("list");
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
            self.record("create");
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
            self.record("update");
            Ok(Record {
                id: id.to_string(),
                data,
            })
        }
        async fn delete(&self, _collection: &str, _id: &str) -> Result<(), DatabaseError> {
            self.record("delete");
            Ok(())
        }
        async fn count(
            &self,
            _collection: &str,
            _filters: &[Filter],
        ) -> Result<i64, DatabaseError> {
            self.record("count");
            Ok(0)
        }
        async fn sum(
            &self,
            _collection: &str,
            _field: &str,
            _filters: &[Filter],
        ) -> Result<f64, DatabaseError> {
            self.record("sum");
            Ok(0.0)
        }
        async fn query_raw(
            &self,
            _query: &str,
            _args: &[serde_json::Value],
        ) -> Result<Vec<Record>, DatabaseError> {
            self.record("query_raw");
            Ok(vec![])
        }
        async fn exec_raw(
            &self,
            _query: &str,
            _args: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            self.record("exec_raw");
            Ok(0)
        }
        async fn delete_where(
            &self,
            _collection: &str,
            _filters: &[Filter],
        ) -> Result<(), DatabaseError> {
            self.record("delete_where");
            Ok(())
        }
        async fn delete_where_count(
            &self,
            _collection: &str,
            _filters: &[Filter],
        ) -> Result<i64, DatabaseError> {
            self.record("delete_where_count");
            Ok(0)
        }
        async fn take_where(
            &self,
            _collection: &str,
            _filters: &[Filter],
        ) -> Result<Vec<Record>, DatabaseError> {
            self.record("take_where");
            Ok(vec![])
        }
        async fn update_where(
            &self,
            _collection: &str,
            _filters: &[Filter],
            _data: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<(), DatabaseError> {
            self.record("update_where");
            Ok(())
        }
        async fn increment_field_where(
            &self,
            _collection: &str,
            _col: &str,
            _delta: i64,
            _filters: &[Filter],
        ) -> Result<i64, DatabaseError> {
            self.record("increment_field_where");
            Ok(0)
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

mod storage_fakes {
    use async_trait::async_trait;
    use wafer_core::interfaces::storage::service::{
        FolderInfo, ListOptions, ObjectInfo, ObjectList, StorageError, StorageService,
    };

    use super::Calls;

    pub struct RecordingStorage {
        pub calls: Calls,
    }

    impl RecordingStorage {
        pub fn new(calls: Calls) -> Self {
            Self { calls }
        }

        fn record(&self, op: &'static str) {
            self.calls.lock().unwrap().push(op);
        }
    }

    #[async_trait]
    impl StorageService for RecordingStorage {
        async fn put(
            &self,
            _folder: &str,
            _key: &str,
            _data: &[u8],
            _content_type: &str,
        ) -> Result<(), StorageError> {
            self.record("put");
            Ok(())
        }
        async fn get(
            &self,
            _folder: &str,
            key: &str,
        ) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
            self.record("get");
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
            self.record("delete");
            Ok(())
        }
        async fn list(
            &self,
            _folder: &str,
            _opts: &ListOptions,
        ) -> Result<ObjectList, StorageError> {
            self.record("list");
            Ok(ObjectList {
                objects: vec![],
                total_count: 0,
            })
        }
        async fn create_folder(&self, _name: &str, _public: bool) -> Result<(), StorageError> {
            self.record("create_folder");
            Ok(())
        }
        async fn delete_folder(&self, _name: &str) -> Result<(), StorageError> {
            self.record("delete_folder");
            Ok(())
        }
        async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
            self.record("list_folders");
            Ok(vec![])
        }
    }
}

mod config_fakes {
    use wafer_core::interfaces::config::service::ConfigService;

    use super::Calls;

    pub struct RecordingConfig {
        pub calls: Calls,
    }

    impl RecordingConfig {
        pub fn new(calls: Calls) -> Self {
            Self { calls }
        }

        fn record(&self, op: &'static str) {
            self.calls.lock().unwrap().push(op);
        }
    }

    impl ConfigService for RecordingConfig {
        fn get(&self, _key: &str) -> Option<String> {
            self.record("get");
            Some("value".into())
        }
        fn set(&self, _key: &str, _value: &str) {
            self.record("set");
        }
    }
}

mod network_fakes {
    use async_trait::async_trait;
    use wafer_core::interfaces::network::service::{
        NetworkError, NetworkService, Request, Response,
    };

    use super::Calls;

    pub struct RecordingNetwork {
        pub calls: Calls,
    }

    impl RecordingNetwork {
        pub fn new(calls: Calls) -> Self {
            Self { calls }
        }
    }

    #[async_trait]
    impl NetworkService for RecordingNetwork {
        async fn do_request(&self, _req: &Request) -> Result<Response, NetworkError> {
            self.calls.lock().unwrap().push("do_request");
            Ok(Response {
                status_code: 200,
                headers: Default::default(),
                body: vec![],
            })
        }
    }
}

mod crypto_fakes {
    use wafer_core::interfaces::crypto::service::{CryptoError, CryptoService};

    use super::Calls;

    pub struct RecordingCrypto {
        pub calls: Calls,
    }

    impl RecordingCrypto {
        pub fn new(calls: Calls) -> Self {
            Self { calls }
        }

        fn record(&self, op: &'static str) {
            self.calls.lock().unwrap().push(op);
        }
    }

    impl CryptoService for RecordingCrypto {
        fn hash(&self, _password: &str) -> Result<String, CryptoError> {
            self.record("hash");
            Ok("hash".into())
        }
        fn compare_hash(&self, _password: &str, _hash: &str) -> Result<(), CryptoError> {
            self.record("compare_hash");
            Ok(())
        }
        fn sign(
            &self,
            _claims: std::collections::HashMap<String, serde_json::Value>,
            _expiry: std::time::Duration,
        ) -> Result<String, CryptoError> {
            self.record("sign");
            Ok("token".into())
        }
        fn verify(
            &self,
            _token: &str,
        ) -> Result<std::collections::HashMap<String, serde_json::Value>, CryptoError> {
            self.record("verify");
            Ok(Default::default())
        }
        fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError> {
            self.record("random_bytes");
            Ok(vec![0; n])
        }
    }
}

// ---------------------------------------------------------------------------
// DenyCtx — models a caller with no WRAP grant for anything. Relies on the
// `Context` trait's fail-closed default (see `wafer-block/src/context.rs`):
// any impl that doesn't override `check_resource_access` gets
// `Err(PermissionDenied)` from every call. This is deliberate — it means the
// completeness guarantee below is exercising the SAME fail-closed default
// that a newly-added, not-yet-wired-up op would inherit, which is exactly
// the scenario this test protects against.
// ---------------------------------------------------------------------------

struct DenyCtx;

#[wafer_block::wafer_async_trait]
impl Context for DenyCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("not exercised by decode_and_authorize / check_resource_access")
    }

    fn is_cancelled(&self) -> bool {
        unimplemented!("not exercised by decode_and_authorize / check_resource_access")
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        unimplemented!("not exercised by decode_and_authorize / check_resource_access")
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        unimplemented!("not exercised by decode_and_authorize / check_resource_access")
    }

    // `check_resource_access` uses the trait's fail-closed default (deny).
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A bare `Message` carrying only `kind` — no `wrap.resource` /
/// `wrap.access` / `wrap.resource_type` meta at all. The exploit shape: a
/// caller (or a compromised WASM guest bypassing the client wrapper) that
/// never stamps WRAP meta.
fn msg_without_wrap_meta(kind: &str) -> Message {
    Message::new(kind)
}

async fn expect_permission_denied(out: OutputStream, op: &str) -> WaferError {
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => {
            assert_eq!(
                e.code,
                wafer_block::ErrorCode::PermissionDenied,
                "op `{op}` expected PERMISSION_DENIED, got {:?}: {}",
                e.code,
                e.message
            );
            e
        }
        other => panic!("op `{op}` expected a PermissionDenied error terminal, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Minimal-body builders — one exhaustive `match` per family over that
// family's op constants. An unmatched op panics: this is what forces a
// future op added to a slice to also get a body-construction case here (or
// the test fails to even build a request, before the deny assertion runs).
// ---------------------------------------------------------------------------

fn database_op_body(op: &str) -> Vec<u8> {
    use wafer_block::wire::database as wire;

    let encoded = match op {
        ServiceOp::DATABASE_GET => codec::encode(&wire::GetRequest {
            collection: "suppers_ai__auth__users".into(),
            id: "1".into(),
        }),
        ServiceOp::DATABASE_LIST => codec::encode(&wire::ListRequest {
            collection: "suppers_ai__auth__users".into(),
            filters: vec![],
            sort: vec![],
            limit: 10,
            offset: 0,
            skip_count: false,
        }),
        ServiceOp::DATABASE_CREATE => codec::encode(&wire::CreateRequest {
            collection: "suppers_ai__auth__users".into(),
            data: HashMap::new(),
        }),
        ServiceOp::DATABASE_UPDATE => codec::encode(&wire::UpdateRequest {
            collection: "suppers_ai__auth__users".into(),
            id: "1".into(),
            data: HashMap::new(),
        }),
        ServiceOp::DATABASE_UPDATE_WHERE => codec::encode(&wire::UpdateWhereRequest {
            collection: "suppers_ai__auth__users".into(),
            filters: vec![],
            data: HashMap::new(),
        }),
        ServiceOp::DATABASE_DELETE => codec::encode(&wire::DeleteRequest {
            collection: "suppers_ai__auth__users".into(),
            id: "1".into(),
        }),
        ServiceOp::DATABASE_DELETE_WHERE => codec::encode(&wire::DeleteWhereRequest {
            collection: "suppers_ai__auth__users".into(),
            filters: vec![],
        }),
        ServiceOp::DATABASE_DELETE_WHERE_COUNT => codec::encode(&wire::DeleteWhereCountRequest {
            collection: "suppers_ai__auth__users".into(),
            filters: vec![],
        }),
        ServiceOp::DATABASE_TAKE_WHERE => codec::encode(&wire::TakeWhereRequest {
            collection: "suppers_ai__auth__users".into(),
            filters: vec![],
        }),
        ServiceOp::DATABASE_COUNT => codec::encode(&wire::CountRequest {
            collection: "suppers_ai__auth__users".into(),
            filters: vec![],
        }),
        ServiceOp::DATABASE_SUM => codec::encode(&wire::SumRequest {
            collection: "suppers_ai__auth__users".into(),
            field: "amount".into(),
            filters: vec![],
        }),
        ServiceOp::DATABASE_INCREMENT_FIELD_WHERE => {
            codec::encode(&wire::IncrementFieldWhereRequest {
                collection: "suppers_ai__auth__users".into(),
                col: "count".into(),
                delta: 1,
                filters: vec![],
            })
        }
        ServiceOp::DATABASE_QUERY => codec::encode(&wire::QueryRequest {
            sql: "SELECT 1".into(),
            args: vec![],
            collection: "suppers_ai__auth__users".into(),
        }),
        ServiceOp::DATABASE_QUERY_RAW => codec::encode(&wire::QueryRawRequest {
            query: "SELECT 1".into(),
            args: vec![],
        }),
        ServiceOp::DATABASE_EXECUTE => codec::encode(&wire::ExecuteRequest {
            sql: "UPDATE suppers_ai__auth__users SET x = 1".into(),
            args: vec![],
            collection: "suppers_ai__auth__users".into(),
        }),
        ServiceOp::DATABASE_EXEC_RAW => codec::encode(&wire::ExecRawRequest {
            query: "DELETE FROM suppers_ai__auth__users".into(),
            args: vec![],
        }),
        ServiceOp::DATABASE_DDL => codec::encode(&wire::ExecRawRequest {
            query: "CREATE TABLE suppers_ai__auth__widgets (id TEXT)".into(),
            args: vec![],
        }),
        other => panic!(
            "completeness test has no minimal-body case for database op `{other}` — \
             add a `match` arm to `database_op_body` so this op stays covered \
             by the completeness guarantee"
        ),
    };
    encoded.expect("encode must succeed")
}

fn storage_op_body(op: &str) -> Vec<u8> {
    use wafer_block::wire::storage as wire;

    let encoded = match op {
        ServiceOp::STORAGE_PUT => codec::encode(&wire::PutRequest {
            folder: "uploads".into(),
            key: "k".into(),
            data: vec![],
            content_type: "application/octet-stream".into(),
        }),
        ServiceOp::STORAGE_GET => codec::encode(&wire::GetRequest {
            folder: "uploads".into(),
            key: "k".into(),
        }),
        ServiceOp::STORAGE_DELETE => codec::encode(&wire::DeleteRequest {
            folder: "uploads".into(),
            key: "k".into(),
        }),
        ServiceOp::STORAGE_LIST => codec::encode(&wire::ListRequest {
            folder: "uploads".into(),
            prefix: String::new(),
            limit: 10,
            offset: 0,
        }),
        ServiceOp::STORAGE_CREATE_FOLDER => codec::encode(&wire::CreateFolderRequest {
            name: "uploads".into(),
            public: false,
        }),
        ServiceOp::STORAGE_DELETE_FOLDER => codec::encode(&wire::DeleteFolderRequest {
            name: "uploads".into(),
        }),
        other => panic!(
            "completeness test has no minimal-body case for storage op `{other}` — \
             add a `match` arm to `storage_op_body` (or, if it's a deliberate \
             residual like STORAGE_LIST_FOLDERS, add it to the documented \
             skip-list in the completeness loop AND a residual-pinning test)"
        ),
    };
    encoded.expect("encode must succeed")
}

fn config_op_body(op: &str) -> Vec<u8> {
    use wafer_block::wire::config as wire;

    let encoded = match op {
        ServiceOp::CONFIG_GET => codec::encode(&wire::GetRequest {
            key: "SOME_KEY".into(),
        }),
        ServiceOp::CONFIG_SET => codec::encode(&wire::SetRequest {
            key: "SOME_KEY".into(),
            value: "v".into(),
        }),
        other => panic!(
            "completeness test has no minimal-body case for config op `{other}` — \
             add a `match` arm to `config_op_body` so this op stays covered \
             by the completeness guarantee"
        ),
    };
    encoded.expect("encode must succeed")
}

fn network_op_body(op: &str) -> Vec<u8> {
    use wafer_block::wire::network as wire;

    let encoded = match op {
        ServiceOp::NETWORK_DO_REQUEST => codec::encode(&wire::Request {
            method: "GET".into(),
            url: "https://example.com".into(),
            headers: HashMap::new(),
            body: None,
        }),
        other => panic!(
            "completeness test has no minimal-body case for network op `{other}` — \
             add a `match` arm to `network_op_body` so this op stays covered \
             by the completeness guarantee"
        ),
    };
    encoded.expect("encode must succeed")
}

fn crypto_op_body(op: &str) -> Vec<u8> {
    use wafer_block::wire::crypto as wire;

    let encoded = match op {
        ServiceOp::CRYPTO_HASH => codec::encode(&wire::HashRequest {
            password: "hunter2".into(),
        }),
        ServiceOp::CRYPTO_COMPARE_HASH => codec::encode(&wire::CompareHashRequest {
            password: "hunter2".into(),
            hash: "$argon2id$...".into(),
        }),
        ServiceOp::CRYPTO_SIGN => codec::encode(&wire::SignRequest {
            claims: HashMap::new(),
            expiry_secs: 3600,
        }),
        ServiceOp::CRYPTO_VERIFY => codec::encode(&wire::VerifyRequest {
            token: "tok".into(),
        }),
        ServiceOp::CRYPTO_RANDOM_BYTES => codec::encode(&wire::RandomBytesRequest { n: 16 }),
        other => panic!(
            "completeness test has no minimal-body case for crypto op `{other}` — \
             add a `match` arm to `crypto_op_body` so this op stays covered \
             by the completeness guarantee"
        ),
    };
    encoded.expect("encode must succeed")
}

// ---------------------------------------------------------------------------
// The completeness loop, one test per family.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn database_ops_all_deny_under_deny_ctx() {
    assert!(
        !ServiceOp::DATABASE_OPS.is_empty(),
        "sanity: DATABASE_OPS must not be empty"
    );
    for op in ServiceOp::DATABASE_OPS {
        let calls = new_calls();
        let svc = db_fakes::RecordingDb::new(calls.clone());
        let body = database_op_body(op);
        let msg = msg_without_wrap_meta(op);

        let out =
            wafer_core::interfaces::database::handler::handle_message(&svc, &DenyCtx, &msg, &body)
                .await;
        expect_permission_denied(out, op).await;

        assert!(
            calls.lock().unwrap().is_empty(),
            "database op `{op}` must not reach the service on a denied request; calls = {:?}",
            calls.lock().unwrap()
        );
    }
}

#[tokio::test]
async fn storage_ops_all_deny_under_deny_ctx_except_documented_residual() {
    assert!(
        !ServiceOp::STORAGE_OPS.is_empty(),
        "sanity: STORAGE_OPS must not be empty"
    );
    for op in ServiceOp::STORAGE_OPS {
        if *op == ServiceOp::STORAGE_LIST_FOLDERS {
            // RESIDUAL: unenforced, tracked as a dedicated follow-up — see the module doc
            // comment at the top of this file and
            // `storage_list_folders_residual_is_currently_unenforced` below,
            // which pins its current (unenforced) behavior.
            continue;
        }

        let calls = new_calls();
        let svc = storage_fakes::RecordingStorage::new(calls.clone());
        let body = storage_op_body(op);
        let msg = msg_without_wrap_meta(op);

        let out =
            wafer_core::interfaces::storage::handler::handle_message(&svc, &DenyCtx, &msg, &body)
                .await;
        expect_permission_denied(out, op).await;

        assert!(
            calls.lock().unwrap().is_empty(),
            "storage op `{op}` must not reach the service on a denied request; calls = {:?}",
            calls.lock().unwrap()
        );
    }
}

/// Pins the CURRENT (unenforced) behavior of `storage.list_folders`: it has
/// no `check_resource_access` call in the handler at all, so it reaches the
/// service and succeeds even under a `Context` that denies everything.
///
/// This is a known, tracked residual (see the module doc comment) — not a
/// silent gap. If this test starts failing because `list_folders` now
/// denies, that's good news: promote it out of the skip-list in
/// `storage_ops_all_deny_under_deny_ctx_except_documented_residual` and
/// delete this pin.
#[tokio::test]
async fn storage_list_folders_residual_is_currently_unenforced() {
    let calls = new_calls();
    let svc = storage_fakes::RecordingStorage::new(calls.clone());
    let msg = msg_without_wrap_meta(ServiceOp::STORAGE_LIST_FOLDERS);

    let out =
        wafer_core::interfaces::storage::handler::handle_message(&svc, &DenyCtx, &msg, &[]).await;
    if let Err(TerminalNotResponse::Error(e)) = out.collect_buffered().await {
        panic!(
            "expected storage.list_folders to currently be UNENFORCED (reach the \
             service even under a denying ctx) — got an error instead: {:?}: {}. \
             If this op is now enforced, remove this pin and drop it from the \
             skip-list in the sibling completeness test.",
            e.code, e.message
        );
    }

    assert_eq!(
        *calls.lock().unwrap(),
        vec!["list_folders"],
        "list_folders should have reached the service — this is the documented residual"
    );
}

#[tokio::test]
async fn config_ops_all_deny_under_deny_ctx() {
    assert!(
        !ServiceOp::CONFIG_OPS.is_empty(),
        "sanity: CONFIG_OPS must not be empty"
    );
    for op in ServiceOp::CONFIG_OPS {
        let calls = new_calls();
        let svc = config_fakes::RecordingConfig::new(calls.clone());
        let body = config_op_body(op);
        let msg = msg_without_wrap_meta(op);

        // `config`'s handler is synchronous (returns `OutputStream` directly,
        // no `.await` on the call itself); the *collection* of that stream is
        // still async, handled inside `expect_permission_denied`.
        let out =
            wafer_core::interfaces::config::handler::handle_message(&svc, &DenyCtx, &msg, &body);
        expect_permission_denied(out, op).await;

        assert!(
            calls.lock().unwrap().is_empty(),
            "config op `{op}` must not reach the service on a denied request; calls = {:?}",
            calls.lock().unwrap()
        );
    }
}

#[tokio::test]
async fn network_ops_all_deny_under_deny_ctx() {
    assert!(
        !ServiceOp::NETWORK_OPS.is_empty(),
        "sanity: NETWORK_OPS must not be empty"
    );
    for op in ServiceOp::NETWORK_OPS {
        let calls = new_calls();
        let svc = network_fakes::RecordingNetwork::new(calls.clone());
        let body = network_op_body(op);
        let msg = msg_without_wrap_meta(op);

        let out =
            wafer_core::interfaces::network::handler::handle_message(&svc, &DenyCtx, &msg, &body)
                .await;
        expect_permission_denied(out, op).await;

        assert!(
            calls.lock().unwrap().is_empty(),
            "network op `{op}` must not reach the service on a denied request; calls = {:?}",
            calls.lock().unwrap()
        );
    }
}

#[tokio::test]
async fn crypto_ops_all_deny_under_deny_ctx() {
    assert!(
        !ServiceOp::CRYPTO_OPS.is_empty(),
        "sanity: CRYPTO_OPS must not be empty"
    );
    for op in ServiceOp::CRYPTO_OPS {
        let calls = new_calls();
        let svc = crypto_fakes::RecordingCrypto::new(calls.clone());
        let body = crypto_op_body(op);
        let msg = msg_without_wrap_meta(op);

        // `crypto`'s handler is synchronous, same shape as `config` above.
        let out = wafer_core::interfaces::crypto::handler::handle_message(
            &svc, &DenyCtx, None, &msg, &body,
        );
        expect_permission_denied(out, op).await;

        assert!(
            calls.lock().unwrap().is_empty(),
            "crypto op `{op}` must not reach the service on a denied request; calls = {:?}",
            calls.lock().unwrap()
        );
    }
}
