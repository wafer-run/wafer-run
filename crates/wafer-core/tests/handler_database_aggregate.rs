//! Handler tests for the `database.aggregate` service op.
//!
//! Covers the handler-side contract that can't be exercised by a direct
//! `DatabaseService::aggregate` call (those real-SQLite shape tests live in
//! `wafer-block-sqlite`):
//! - happy path: a granting `Context` + a valid request reaches the service
//!   and the returned `Vec<Record>` is encoded back on the wire;
//! - authorization: a denying `Context` returns `PERMISSION_DENIED` (the
//!   *non-execution* half of this is pinned by `handler_wrap_completeness`,
//!   which asserts the service is never reached under a denying ctx);
//! - validation (all `InvalidArgument`, fail-closed, before the service runs):
//!   an empty `aggregates` list, a hostile identifier reaching raw SQL text
//!   (alias / group-by column / aggregated field / date-bucket field), an
//!   empty `CaseWhenSum.when`, and a filter *group* (aggregation filters are
//!   AND-of-leaves, same rule as `count`/`sum`).

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
// Fake service — `aggregate` returns a canned single row; everything else is
// minimal. The validation/deny tests never reach it; the happy-path test
// asserts its canned row round-trips back through the handler.
// ---------------------------------------------------------------------------

mod db_fakes {
    use async_trait::async_trait;
    use wafer_block::db::{Filter, ListOptions};
    use wafer_core::interfaces::database::service::{
        AggregateSpec, DatabaseError, DatabaseService, Record, RecordList, UpsertSpec,
    };
    use wafer_schema::{Column, Table};

    pub struct AggDb;

    #[async_trait]
    impl DatabaseService for AggDb {
        async fn aggregate(
            &self,
            _collection: &str,
            _spec: AggregateSpec,
        ) -> Result<Vec<Record>, DatabaseError> {
            let mut data = std::collections::HashMap::new();
            data.insert("status".to_string(), serde_json::json!("active"));
            data.insert("cnt".to_string(), serde_json::json!(7));
            Ok(vec![Record {
                id: String::new(),
                data,
            }])
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
        async fn upsert(&self, _collection: &str, _spec: UpsertSpec) -> Result<i64, DatabaseError> {
            Err(DatabaseError::Internal("fixture: upsert not needed".into()))
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
    Message::new(ServiceOp::DATABASE_AGGREGATE)
}

async fn dispatch(ctx: &dyn Context, req: &wire::AggregateRequest) -> OutputStream {
    let body = codec::encode(req).unwrap();
    wafer_core::interfaces::database::handler::handle_message(&db_fakes::AggDb, ctx, &msg(), &body)
        .await
}

async fn terminal_error(out: OutputStream) -> Option<WaferError> {
    match out.collect_buffered().await {
        Ok(_) => None,
        Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => Some(e),
        _ => None,
    }
}

async fn expect_invalid(out: OutputStream, ctx_note: &str) {
    let err = terminal_error(out)
        .await
        .unwrap_or_else(|| panic!("{ctx_note}: expected an InvalidArgument error, got success"));
    assert_eq!(
        err.code,
        ErrorCode::InvalidArgument,
        "{ctx_note}: expected INVALID_ARGUMENT, got {:?}: {}",
        err.code,
        err.message
    );
}

fn count_agg(alias: &str) -> wire::AggregateColumnDef {
    wire::AggregateColumnDef::Count {
        alias: alias.into(),
    }
}

// ---------------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn aggregate_with_grant_returns_rows() {
    let req = wire::AggregateRequest {
        collection: "my_org__auth__users".into(),
        select_columns: vec!["status".into()],
        aggregates: vec![count_agg("cnt")],
        filters: vec![],
        group_by: vec![wire::GroupByDef::Column("status".into())],
        sort: vec![],
        limit: 0,
    };
    let out = dispatch(&AllowCtx, &req).await;
    let buffered = match out.collect_buffered().await {
        Ok(b) => b,
        other => panic!("expected a response, got {other:?}"),
    };
    let rows: Vec<wire::Record> = codec::decode(&buffered.body).expect("decode response");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].data["status"], serde_json::json!("active"));
    assert_eq!(rows[0].data["cnt"], serde_json::json!(7));
}

// ---------------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------------

#[tokio::test]
async fn aggregate_without_grant_returns_permission_denied() {
    let req = wire::AggregateRequest {
        collection: "my_org__auth__orders".into(),
        select_columns: vec![],
        aggregates: vec![count_agg("cnt")],
        filters: vec![],
        group_by: vec![],
        sort: vec![],
        limit: 0,
    };
    let out = dispatch(&DenyCtx, &req).await;
    let err = terminal_error(out)
        .await
        .expect("expected PERMISSION_DENIED");
    assert_eq!(err.code, ErrorCode::PermissionDenied, "{}", err.message);
    assert!(
        err.message.contains("my_org__auth__orders"),
        "error should name the denied collection; got: {}",
        err.message
    );
}

// ---------------------------------------------------------------------------
// Validation (all InvalidArgument, before the service runs)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn aggregate_empty_aggregates_is_invalid_argument() {
    let req = wire::AggregateRequest {
        collection: "my_org__auth__users".into(),
        select_columns: vec![],
        aggregates: vec![],
        filters: vec![],
        group_by: vec![],
        sort: vec![],
        limit: 0,
    };
    expect_invalid(dispatch(&AllowCtx, &req).await, "empty aggregates").await;
}

#[tokio::test]
async fn aggregate_bad_alias_is_invalid_argument() {
    let req = wire::AggregateRequest {
        collection: "my_org__auth__users".into(),
        select_columns: vec![],
        aggregates: vec![count_agg("cnt); DROP TABLE users;--")],
        filters: vec![],
        group_by: vec![],
        sort: vec![],
        limit: 0,
    };
    expect_invalid(dispatch(&AllowCtx, &req).await, "hostile alias").await;
}

#[tokio::test]
async fn aggregate_bad_group_by_column_is_invalid_argument() {
    let req = wire::AggregateRequest {
        collection: "my_org__auth__users".into(),
        select_columns: vec![],
        aggregates: vec![count_agg("cnt")],
        filters: vec![],
        group_by: vec![wire::GroupByDef::Column("status\") --".into())],
        sort: vec![],
        limit: 0,
    };
    expect_invalid(dispatch(&AllowCtx, &req).await, "hostile group-by column").await;
}

#[tokio::test]
async fn aggregate_bad_sum_field_is_invalid_argument() {
    let req = wire::AggregateRequest {
        collection: "my_org__auth__users".into(),
        select_columns: vec![],
        aggregates: vec![wire::AggregateColumnDef::Sum {
            field: "amount) FROM x --".into(),
            alias: "total".into(),
        }],
        filters: vec![],
        group_by: vec![],
        sort: vec![],
        limit: 0,
    };
    expect_invalid(dispatch(&AllowCtx, &req).await, "hostile sum field").await;
}

#[tokio::test]
async fn aggregate_bad_max_field_is_invalid_argument() {
    let req = wire::AggregateRequest {
        collection: "my_org__auth__users".into(),
        select_columns: vec![],
        aggregates: vec![wire::AggregateColumnDef::Max {
            field: "value) FROM x --".into(),
            alias: "max_val".into(),
        }],
        filters: vec![],
        group_by: vec![],
        sort: vec![],
        limit: 0,
    };
    expect_invalid(dispatch(&AllowCtx, &req).await, "hostile max field").await;
}

#[tokio::test]
async fn aggregate_bad_date_bucket_field_is_invalid_argument() {
    let req = wire::AggregateRequest {
        collection: "my_org__auth__users".into(),
        select_columns: vec![],
        aggregates: vec![count_agg("cnt")],
        filters: vec![],
        group_by: vec![wire::GroupByDef::DateBucket {
            field: "created_at\") OR 1=1 --".into(),
        }],
        sort: vec![],
        limit: 0,
    };
    expect_invalid(dispatch(&AllowCtx, &req).await, "hostile date-bucket field").await;
}

#[tokio::test]
async fn aggregate_empty_case_when_is_invalid_argument() {
    let req = wire::AggregateRequest {
        collection: "my_org__auth__users".into(),
        select_columns: vec![],
        aggregates: vec![wire::AggregateColumnDef::CaseWhenSum {
            when: vec![],
            alias: "errors".into(),
        }],
        filters: vec![],
        group_by: vec![],
        sort: vec![],
        limit: 0,
    };
    expect_invalid(dispatch(&AllowCtx, &req).await, "empty case-when predicate").await;
}

#[tokio::test]
async fn aggregate_filter_group_is_invalid_argument() {
    // Aggregation filters are AND-of-leaves; a group node is rejected, same as
    // count/sum.
    let req = wire::AggregateRequest {
        collection: "my_org__auth__users".into(),
        select_columns: vec![],
        aggregates: vec![count_agg("cnt")],
        filters: vec![wire::FilterNode::Any {
            any: vec![wire::FilterNode::Leaf(wire::FilterDef {
                field: "status".into(),
                operator: "eq".into(),
                value: serde_json::json!("active"),
            })],
        }],
        group_by: vec![],
        sort: vec![],
        limit: 0,
    };
    expect_invalid(dispatch(&AllowCtx, &req).await, "filter group").await;
}
