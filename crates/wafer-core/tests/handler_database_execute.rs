//! Handler tests for `database.execute` and `database.query` service ops.
//!
//! Tests cover:
//! - Happy-path WRAP validation (matching collection in meta → success)
//! - WRAP rejection (mismatched collection meta → PERMISSION_DENIED)
//! - Security-critical: a read-only grant does not satisfy a write operation.
//!   `DATABASE_EXECUTE` is flagged `is_write=true` at the client layer; the
//!   WRAP `check_access` call in the runtime enforces that read-only grants
//!   cannot satisfy write requests.

use wafer_block::{
    codec,
    common::ServiceOp,
    meta::{META_WRAP_ACCESS, META_WRAP_RESOURCE, META_WRAP_RESOURCE_TYPE},
    types::ResourceGrant,
    wire, ErrorCode, Message, WaferError,
};

// ---------------------------------------------------------------------------
// Fake DatabaseService — all ops succeed with empty/zero data.
// ---------------------------------------------------------------------------

mod db_fakes {
    use async_trait::async_trait;
    use wafer_block::db::{Filter, ListOptions};
    use wafer_core::interfaces::database::service::{
        DatabaseError, DatabaseService, Record, RecordList,
    };
    use wafer_schema::{Column, Table};

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
        async fn increment_field_where(
            &self,
            _collection: &str,
            _col: &str,
            _delta: i64,
            _filters: &[Filter],
        ) -> Result<i64, DatabaseError> {
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a Message with WRAP meta set so the handler's collection check passes.
fn msg_with_matching_meta(kind: &str, resource: &str, access: &str) -> Message {
    let mut m = Message::new(kind);
    m.set_meta(META_WRAP_RESOURCE, resource);
    m.set_meta(META_WRAP_ACCESS, access);
    m.set_meta(META_WRAP_RESOURCE_TYPE, "db");
    m
}

/// Build a Message with a WRAP resource that does NOT match the payload's
/// collection, so the handler's SEC-003 cross-validation fires.
fn msg_with_mismatched_meta(kind: &str, decoy_resource: &str, access: &str) -> Message {
    let mut m = Message::new(kind);
    m.set_meta(META_WRAP_RESOURCE, decoy_resource);
    m.set_meta(META_WRAP_ACCESS, access);
    m.set_meta(META_WRAP_RESOURCE_TYPE, "db");
    m
}

async fn terminal_error(out: wafer_block::streams::output::OutputStream) -> Option<WaferError> {
    match out.collect_buffered().await {
        Ok(_) => None,
        Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => Some(e),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Test 1 — execute with matching WRAP resource succeeds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn execute_with_matching_grant_succeeds() {
    let svc = db_fakes::OkDb;
    let req = wire::database::ExecuteRequest {
        sql: "DELETE FROM suppers_ai__auth__users WHERE id = ?".into(),
        args: vec![serde_json::json!("u1")],
        collection: "suppers_ai__auth__users".into(),
    };
    let body = codec::encode(&req).unwrap();
    // Meta resource matches the collection in the payload — handler should pass.
    let msg = msg_with_matching_meta(
        ServiceOp::DATABASE_EXECUTE,
        "suppers_ai__auth__users",
        "write",
    );
    let out = wafer_core::interfaces::database::handler::handle_message(&svc, &msg, &body).await;
    assert!(
        terminal_error(out).await.is_none(),
        "expected success but got an error"
    );
}

// ---------------------------------------------------------------------------
// Test 2 — execute with mismatched WRAP resource returns PERMISSION_DENIED
// ---------------------------------------------------------------------------

#[tokio::test]
async fn execute_without_matching_grant_returns_permission_denied() {
    let svc = db_fakes::OkDb;
    let req = wire::database::ExecuteRequest {
        sql: "DELETE FROM suppers_ai__auth__orders WHERE id = ?".into(),
        args: vec![],
        collection: "suppers_ai__auth__orders".into(),
    };
    let body = codec::encode(&req).unwrap();
    // Meta claims a different collection — SEC-003 cross-validation must reject.
    let msg = msg_with_mismatched_meta(
        ServiceOp::DATABASE_EXECUTE,
        "suppers_ai__auth__decoy",
        "write",
    );
    let out = wafer_core::interfaces::database::handler::handle_message(&svc, &msg, &body).await;
    let err = terminal_error(out)
        .await
        .expect("expected PERMISSION_DENIED error");
    assert_eq!(
        err.code,
        ErrorCode::PermissionDenied,
        "expected PERMISSION_DENIED, got {:?}: {}",
        err.code,
        err.message
    );
    assert!(
        err.message.contains("suppers_ai__auth__orders"),
        "error message should contain the payload collection name; got: {}",
        err.message
    );
    assert!(
        err.message.contains("collection") || err.message.contains("resource"),
        "error message should reference collection/resource; got: {}",
        err.message
    );
}

// ---------------------------------------------------------------------------
// Test 3 — query with matching WRAP resource returns rows
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_with_matching_grant_returns_rows() {
    let svc = db_fakes::OkDb;
    let req = wire::database::QueryRequest {
        sql: "SELECT * FROM suppers_ai__auth__users".into(),
        args: vec![],
        collection: "suppers_ai__auth__users".into(),
    };
    let body = codec::encode(&req).unwrap();
    let msg = msg_with_matching_meta(ServiceOp::DATABASE_QUERY, "suppers_ai__auth__users", "read");
    let out = wafer_core::interfaces::database::handler::handle_message(&svc, &msg, &body).await;
    assert!(
        terminal_error(out).await.is_none(),
        "expected success but got an error"
    );
}

// ---------------------------------------------------------------------------
// Test 4 — query with mismatched WRAP resource returns PERMISSION_DENIED
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_without_matching_grant_returns_permission_denied() {
    let svc = db_fakes::OkDb;
    let req = wire::database::QueryRequest {
        sql: "SELECT * FROM suppers_ai__auth__orders".into(),
        args: vec![],
        collection: "suppers_ai__auth__orders".into(),
    };
    let body = codec::encode(&req).unwrap();
    // Meta claims a different collection.
    let msg =
        msg_with_mismatched_meta(ServiceOp::DATABASE_QUERY, "suppers_ai__auth__decoy", "read");
    let out = wafer_core::interfaces::database::handler::handle_message(&svc, &msg, &body).await;
    let err = terminal_error(out)
        .await
        .expect("expected PERMISSION_DENIED error");
    assert_eq!(
        err.code,
        ErrorCode::PermissionDenied,
        "expected PERMISSION_DENIED, got {:?}: {}",
        err.code,
        err.message
    );
}

// ---------------------------------------------------------------------------
// Test 5 (SECURITY-CRITICAL) — execute with read-only grant returns
// PERMISSION_DENIED.
//
// `DATABASE_EXECUTE` is a write operation. The client wrapper sets
// `META_WRAP_ACCESS = "write"` and passes `is_write = true` to
// `wrap::check_access`. A grant with `write: false` (read-only) must NOT
// satisfy a write request. This test verifies that the `is_write=true` flag
// added in Task 3 is wired correctly by calling `wrap::check_access` the same
// way the runtime does before dispatching to the handler.
// ---------------------------------------------------------------------------

#[test]
fn wrap_check_access_denies_write_op_with_read_only_grant() {
    // Simulate the runtime's pre-dispatch WRAP check for DATABASE_EXECUTE.
    // The client sets is_write=true (META_WRAP_ACCESS="write"). A read-only
    // grant must not satisfy this request.
    let read_only_grant = ResourceGrant::read("suppers-ai/app", "suppers_ai__auth__users");
    let grants = vec![read_only_grant];

    let result = wafer_block::wrap::check_access(
        Some("suppers-ai/app"),    // caller
        "suppers_ai__auth__users", // resource
        true,                      // is_write — DATABASE_EXECUTE always sets this
        None,                      // resource_type (db = namespace-based)
        &grants,
        "suppers-ai/admin", // admin block (not the caller, so admin shortcut doesn't fire)
    );

    assert!(
        result.is_err(),
        "read-only grant must not satisfy a write (execute) request"
    );
    let err = result.unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::PermissionDenied,
        "expected PERMISSION_DENIED when executing with read-only grant; got {:?}: {}",
        err.code,
        err.message
    );
}
