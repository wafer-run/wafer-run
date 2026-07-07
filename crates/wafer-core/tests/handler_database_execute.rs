//! Handler tests for `database.execute` and `database.query` service ops.
//!
//! Tests cover:
//! - Happy-path authorization (a `Context` granting the request's collection
//!   → success)
//! - Authorization rejection (a `Context` denying the request's collection →
//!   PERMISSION_DENIED)
//! - Security-critical: a read-only grant does not satisfy a write operation.
//!   `DATABASE_EXECUTE` is flagged `is_write=true` at the client layer; the
//!   WRAP `check_access` call in the runtime enforces that read-only grants
//!   cannot satisfy write requests.
//!
//! Historical note: before the handler was routed through
//! `decode_and_authorize` (host-side `ctx.check_resource_access`), these ops
//! authorized by cross-validating the caller-supplied `wrap.resource` meta
//! against the decoded payload's `collection` (SEC-003). The database
//! handler no longer reads that meta at all — `msg` is retained only for
//! `msg.kind` dispatch — so these tests now drive authorization through a
//! fake `Context` instead of message meta.

use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    types::{ResourceGrant, ResourceType},
    wire, ErrorCode, Message, WaferError,
};

/// `Context` stub that grants every resource access check.
struct AllowCtx;

#[wafer_block::wafer_async_trait]
impl Context for AllowCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn is_cancelled(&self) -> bool {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn check_resource_access(
        &self,
        _resource: &str,
        _resource_type: ResourceType,
        _is_write: bool,
    ) -> Result<(), WaferError> {
        Ok(())
    }
}

/// `Context` stub that denies every resource access check, mirroring a
/// caller with no WRAP grant for the requested resource. The error message
/// names the resource, matching the operator-friendly wording the runtime's
/// real `check_access` produces.
struct DenyCtx;

#[wafer_block::wafer_async_trait]
impl Context for DenyCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn is_cancelled(&self) -> bool {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn check_resource_access(
        &self,
        resource: &str,
        _resource_type: ResourceType,
        _is_write: bool,
    ) -> Result<(), WaferError> {
        Err(WaferError::new(
            ErrorCode::PermissionDenied,
            format!("WRAP: no grant for resource '{resource}'"),
        ))
    }
}

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

/// Build a bare Message for the given op kind. The database handler no
/// longer reads any WRAP meta off `msg` — authorization runs entirely
/// through the `Context` passed to `handle_message` — so `msg` only needs
/// to carry `kind` for dispatch.
fn msg(kind: &str) -> Message {
    Message::new(kind)
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
    // AllowCtx grants every resource — handler should pass.
    let msg = msg(ServiceOp::DATABASE_EXECUTE);
    let out =
        wafer_core::interfaces::database::handler::handle_message(&svc, &AllowCtx, &msg, &body)
            .await;
    assert!(
        terminal_error(out).await.is_none(),
        "expected success but got an error"
    );
}

// ---------------------------------------------------------------------------
// Test 2 — execute with a denying Context returns PERMISSION_DENIED
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
    // DenyCtx has no grant for the request's collection — must reject.
    let msg = msg(ServiceOp::DATABASE_EXECUTE);
    let out =
        wafer_core::interfaces::database::handler::handle_message(&svc, &DenyCtx, &msg, &body)
            .await;
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
    let msg = msg(ServiceOp::DATABASE_QUERY);
    let out =
        wafer_core::interfaces::database::handler::handle_message(&svc, &AllowCtx, &msg, &body)
            .await;
    assert!(
        terminal_error(out).await.is_none(),
        "expected success but got an error"
    );
}

// ---------------------------------------------------------------------------
// Test 4 — query with a denying Context returns PERMISSION_DENIED
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
    // DenyCtx has no grant for the request's collection.
    let msg = msg(ServiceOp::DATABASE_QUERY);
    let out =
        wafer_core::interfaces::database::handler::handle_message(&svc, &DenyCtx, &msg, &body)
            .await;
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
