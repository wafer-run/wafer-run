//! Handler tests for the `database.update_where_count` service op.
//!
//! Covers the handler-side round trip that can't be exercised by a direct
//! `DatabaseService::update_where_count` call (the real-SQLite affected-row-
//! count shape is exercised end-to-end by the shared `DbExec::update_where_count`
//! impl in `wafer-core/src/interfaces/database/exec.rs`, backed by the
//! `sqlite`/`postgres` `DatabaseService` delegations):
//! - happy path: a granting `Context` + a valid request reaches the service,
//!   the service's real (fixture-backed) filter match count is decoded back
//!   off the wire as `UpdateWhereCountResponse.count`;
//! - the same request decoded through the wire filter conversion, applied
//!   against a *different* filter that matches nothing, returns `count: 0`
//!   (the CAS "already claimed" shape);
//! - authorization: a denying `Context` returns `PERMISSION_DENIED` (the
//!   *non-execution* half of this is pinned by `handler_wrap_completeness`,
//!   which asserts the service is never reached under a denying ctx).

use std::collections::HashMap;

use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    types::ResourceType,
    wire::database as wire,
    ErrorCode, Message, WaferError,
};

// ---------------------------------------------------------------------------
// Contexts
// ---------------------------------------------------------------------------

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
// Fake service — `update_where_count` matches a fixed 3-row fixture
// (status: active, active, inactive) against the incoming `Filter`s using an
// equality-only matcher, so the handler round trip actually proves "count ==
// number of matching rows" rather than echoing a canned value. Everything
// else is minimal, mirroring `handler_database_aggregate.rs`'s `AggDb`.
// ---------------------------------------------------------------------------

mod db_fakes {
    use async_trait::async_trait;
    use wafer_block::db::{Filter, FilterOp, ListOptions};
    use wafer_core::interfaces::database::service::{
        DatabaseError, DatabaseService, Record, RecordList,
    };
    use wafer_schema::{Column, Table};

    pub struct FixtureDb;

    impl FixtureDb {
        /// Three fixture rows' `status` values.
        const STATUSES: [&'static str; 3] = ["active", "active", "inactive"];
    }

    #[async_trait]
    impl DatabaseService for FixtureDb {
        async fn update_where_count(
            &self,
            _collection: &str,
            filters: &[Filter],
            _data: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<i64, DatabaseError> {
            let matches = Self::STATUSES
                .iter()
                .filter(|status| {
                    filters.iter().all(|f| match f.operator {
                        FilterOp::Equal => {
                            f.field == "status" && f.value == serde_json::json!(*status)
                        }
                        _ => false,
                    })
                })
                .count();
            Ok(matches as i64)
        }

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

fn msg() -> Message {
    Message::new(ServiceOp::DATABASE_UPDATE_WHERE_COUNT)
}

async fn dispatch(ctx: &dyn Context, req: &wire::UpdateWhereCountRequest) -> OutputStream {
    let body = codec::encode(req).unwrap();
    wafer_core::interfaces::database::handler::handle_message(
        &db_fakes::FixtureDb,
        ctx,
        &msg(),
        &body,
    )
    .await
}

async fn terminal_error(out: OutputStream) -> Option<WaferError> {
    match out.collect_buffered().await {
        Ok(_) => None,
        Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => Some(e),
        _ => None,
    }
}

fn eq_filter(field: &str, value: &str) -> wire::FilterNode {
    wire::FilterNode::Leaf(wire::FilterDef {
        field: field.into(),
        operator: "eq".into(),
        value: serde_json::json!(value),
    })
}

// ---------------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_where_count_with_grant_returns_matching_row_count() {
    let mut data = HashMap::new();
    data.insert("status".to_string(), serde_json::json!("archived"));
    let req = wire::UpdateWhereCountRequest {
        collection: "my_org__purchases__orders".into(),
        filters: vec![eq_filter("status", "active")],
        data,
    };
    let out = dispatch(&AllowCtx, &req).await;
    let buffered = match out.collect_buffered().await {
        Ok(b) => b,
        other => panic!("expected a response, got {other:?}"),
    };
    let resp: wire::UpdateWhereCountResponse = codec::decode(&buffered.body).expect("decode");
    assert_eq!(resp.count, 2, "two fixture rows have status=active");
}

#[tokio::test]
async fn update_where_count_zero_when_no_rows_match() {
    let mut data = HashMap::new();
    data.insert("status".to_string(), serde_json::json!("archived"));
    let req = wire::UpdateWhereCountRequest {
        collection: "my_org__purchases__orders".into(),
        filters: vec![eq_filter("status", "pending")],
        data,
    };
    let out = dispatch(&AllowCtx, &req).await;
    let buffered = match out.collect_buffered().await {
        Ok(b) => b,
        other => panic!("expected a response, got {other:?}"),
    };
    let resp: wire::UpdateWhereCountResponse = codec::decode(&buffered.body).expect("decode");
    assert_eq!(
        resp.count, 0,
        "no fixture row has status=pending — this is the CAS 'already claimed' shape"
    );
}

// ---------------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_where_count_without_grant_returns_permission_denied() {
    let req = wire::UpdateWhereCountRequest {
        collection: "my_org__purchases__orders".into(),
        filters: vec![eq_filter("status", "active")],
        data: HashMap::new(),
    };
    let out = dispatch(&DenyCtx, &req).await;
    let err = terminal_error(out)
        .await
        .expect("expected PERMISSION_DENIED");
    assert_eq!(err.code, ErrorCode::PermissionDenied, "{}", err.message);
    assert!(
        err.message.contains("my_org__purchases__orders"),
        "error should name the denied collection; got: {}",
        err.message
    );
}
