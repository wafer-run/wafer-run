//! Shared test fakes for the database handler integration test suites.
//!
//! `RecordingDb` is a fake `DatabaseService` that records which trait method
//! ran, so a test can assert a denied request never reached the service —
//! not just that the handler returned the right error. `Calls`/`new_calls`
//! is the shared call log; `msg_without_wrap_meta` builds the exact
//! "meta-omission" message shape (no `wrap.*` meta at all); and
//! `expect_permission_denied` asserts a handler output terminated in
//! `PermissionDenied`.
//!
//! Shared (via `mod common;`) between `handler_database_wrap_authorization.rs`
//! and `handler_database_schema_ops.rs` so the fake and its assertions stay
//! in exact lockstep across both suites. Not every consuming file uses every
//! export.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use wafer_block::{
    db::{Filter, ListOptions},
    streams::output::{OutputStream, TerminalNotResponse},
    ErrorCode, Message, WaferError,
};
use wafer_core::interfaces::database::service::{
    AggregateSpec, DatabaseError, DatabaseService, Record, RecordList, UpsertSpec,
};
use wafer_schema::{Column, Table};

/// Shared call log, checked via `Arc::clone` from the test after the handler
/// call returns.
pub type Calls = Arc<Mutex<Vec<&'static str>>>;

pub fn new_calls() -> Calls {
    Arc::new(Mutex::new(Vec::new()))
}

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
    async fn count(&self, _collection: &str, _filters: &[Filter]) -> Result<i64, DatabaseError> {
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
    async fn upsert(&self, _collection: &str, _spec: UpsertSpec) -> Result<i64, DatabaseError> {
        self.record("upsert");
        Ok(1)
    }
    async fn aggregate(
        &self,
        _collection: &str,
        _spec: AggregateSpec,
    ) -> Result<Vec<Record>, DatabaseError> {
        self.record("aggregate");
        Ok(vec![])
    }
    async fn ensure_schema_table(&self, _table: &Table) -> Result<(), DatabaseError> {
        self.record("ensure_schema_table");
        Ok(())
    }
    async fn schema_table_exists(&self, _name: &str) -> Result<bool, DatabaseError> {
        self.record("schema_table_exists");
        Ok(true)
    }
    async fn schema_drop_table(&self, _name: &str) -> Result<(), DatabaseError> {
        self.record("schema_drop_table");
        Ok(())
    }
    async fn schema_add_column(&self, _table: &str, _column: &Column) -> Result<(), DatabaseError> {
        self.record("schema_add_column");
        Ok(())
    }
}

/// A bare `Message` carrying only `kind` — no `wrap.resource` /
/// `wrap.access` / `wrap.resource_type` meta at all. This is the exact
/// "meta-omission" shape: a caller (or a compromised WASM guest bypassing
/// the client wrapper) that never stamps WRAP meta.
pub fn msg_without_wrap_meta(kind: &str) -> Message {
    Message::new(kind)
}

pub async fn expect_permission_denied(out: OutputStream) -> WaferError {
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => {
            assert_eq!(
                e.code,
                ErrorCode::PermissionDenied,
                "expected PERMISSION_DENIED, got {:?}: {}",
                e.code,
                e.message
            );
            e
        }
        other => panic!("expected a PermissionDenied error terminal, got {other:?}"),
    }
}
