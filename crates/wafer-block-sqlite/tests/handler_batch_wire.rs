//! `database.create_many` and `database.batch` driven end to end: request
//! bytes encoded from a plain JSON-shaped value (what any peer puts on the
//! wire, independent of this build's Rust types) → the shared database
//! handler → the real SQLite service → the response decoded as plain JSON, so
//! the encoding of the per-op results is pinned too.

use std::collections::HashMap;

use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::{ResourceAccess, ResourceType},
    wire::database::MAX_BATCH_WRITES,
    ErrorCode, Message, WaferError,
};
use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::interfaces::database::{
    handler::handle_message,
    service::{pk, Column, DataType, DatabaseService, Table},
};

/// A `Context` that grants every resource, so the requests reach the service.
struct AllowCtx;

#[wafer_block::wafer_async_trait]
impl Context for AllowCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("the database handler makes no block calls")
    }
    fn is_cancelled(&self) -> bool {
        false
    }
    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }
    fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
        unimplemented!("the database handler does not clone its context")
    }
    fn check_resource_access(
        &self,
        _resource: &str,
        _resource_type: ResourceType,
        _access: ResourceAccess,
    ) -> Result<(), WaferError> {
        Ok(())
    }
    fn resource_access_admitted(
        &self,
        _resource: &str,
        _resource_type: ResourceType,
        _access: ResourceAccess,
    ) -> bool {
        true
    }
}

const TABLE: &str = "items";

async fn seeded() -> SQLiteDatabaseService {
    let svc = SQLiteDatabaseService::open_in_memory().expect("open in-memory sqlite");
    svc.ensure_schema_table(&Table {
        name: TABLE.to_string(),
        columns: vec![
            pk("id"),
            Column::new("name", DataType::Text).null(),
            Column::new("kind", DataType::Text).null(),
            Column::new("qty", DataType::Int).null(),
        ],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    })
    .await
    .expect("create items");
    for (id, kind) in [("i1", "a"), ("i2", "a"), ("i3", "b")] {
        let data: HashMap<String, serde_json::Value> = [
            ("id", serde_json::json!(id)),
            ("name", serde_json::json!("orig")),
            ("kind", serde_json::json!(kind)),
            ("qty", serde_json::json!(1)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        svc.create(TABLE, data).await.expect("seed item");
    }
    svc
}

async fn dispatch(
    svc: &SQLiteDatabaseService,
    op: &str,
    request: &serde_json::Value,
) -> Result<serde_json::Value, WaferError> {
    let body = codec::encode(request).expect("encode request");
    match handle_message(svc, &AllowCtx, &Message::new(op), &body)
        .await
        .collect_buffered()
        .await
    {
        Ok(resp) => Ok(codec::decode(&resp.body).expect("decode response as plain JSON")),
        Err(TerminalNotResponse::Error(e)) => Err(e),
        Err(_) => panic!("{op}: the handler ended the stream without a response or error"),
    }
}

async fn count(svc: &SQLiteDatabaseService) -> i64 {
    svc.count(TABLE, &[]).await.expect("count")
}

#[tokio::test]
async fn create_many_inserts_every_row_off_the_wire() {
    let svc = seeded().await;
    let rows: Vec<serde_json::Value> = (0..50)
        .map(|i| serde_json::json!({ "name": format!("n{i}"), "qty": i }))
        .collect();
    let resp = dispatch(
        &svc,
        ServiceOp::DATABASE_CREATE_MANY,
        &serde_json::json!({ "collection": TABLE, "rows": rows }),
    )
    .await
    .expect("create_many");
    assert_eq!(resp, serde_json::json!({ "rows_affected": 50 }));
    assert_eq!(count(&svc).await, 53);
}

#[tokio::test]
async fn create_many_with_a_duplicate_key_inserts_nothing() {
    let svc = seeded().await;
    let err = dispatch(
        &svc,
        ServiceOp::DATABASE_CREATE_MANY,
        &serde_json::json!({
            "collection": TABLE,
            "rows": [{ "id": "new1" }, { "id": "i1" }, { "id": "new2" }],
        }),
    )
    .await
    .expect_err("the duplicate key fails the call");
    assert_eq!(err.code, ErrorCode::AlreadyExists, "{}", err.message);
    assert_eq!(count(&svc).await, 3, "new1 was rolled back");
}

#[tokio::test]
async fn batch_applies_mixed_writes_and_reports_each_outcome() {
    let svc = seeded().await;
    let resp = dispatch(
        &svc,
        ServiceOp::DATABASE_BATCH,
        &serde_json::json!({ "ops": [
            { "Create": { "collection": TABLE, "data": { "id": "i4", "name": "new" } } },
            { "Update": { "collection": TABLE, "id": "i1", "data": { "qty": 9 } } },
            { "Delete": { "collection": TABLE, "id": "i2" } },
            { "UpdateWhere": {
                "collection": TABLE,
                "filters": [{ "field": "kind", "value": "b" }],
                "data": { "name": "Y" },
            } },
            { "Upsert": {
                "collection": TABLE,
                "data": [["id", "i5"], ["name", "up"]],
                "conflict_columns": ["id"],
                "on_conflict": { "SetColumns": ["name"] },
            } },
            { "Update": { "collection": TABLE, "id": "missing", "data": { "qty": 0 } } },
        ] }),
    )
    .await
    .expect("batch");

    let results = resp["results"].as_array().expect("results array");
    assert_eq!(results.len(), 6, "{resp}");
    assert_eq!(results[0]["Created"]["id"], "i4", "{resp}");
    assert_eq!(results[0]["Created"]["data"]["name"], "new", "{resp}");
    assert_eq!(results[1]["Updated"]["id"], "i1", "{resp}");
    assert_eq!(results[1]["Updated"]["data"]["qty"], 9, "{resp}");
    assert_eq!(
        results[2],
        serde_json::json!({ "Deleted": { "rows_affected": 1 } })
    );
    assert_eq!(
        results[3],
        serde_json::json!({ "UpdatedWhere": { "rows_affected": 1 } })
    );
    assert_eq!(
        results[4],
        serde_json::json!({ "Upserted": { "rows_affected": 1 } })
    );
    assert_eq!(results[5], serde_json::json!({ "Updated": null }));

    assert_eq!(count(&svc).await, 4, "i1 i3 i4 i5");
    assert_eq!(
        svc.get(TABLE, "i3").await.expect("i3").data["name"],
        serde_json::json!("Y")
    );
}

/// A batch whose third op is invalid (a column-to-column filter where the
/// update-where family takes flat value filters) is refused as a whole before
/// any SQL runs — the two valid writes ahead of it never happen.
#[tokio::test]
async fn an_invalid_op_rejects_the_whole_batch_before_any_write() {
    let svc = seeded().await;
    let err = dispatch(
        &svc,
        ServiceOp::DATABASE_BATCH,
        &serde_json::json!({ "ops": [
            { "Create": { "collection": TABLE, "data": { "id": "i4" } } },
            { "Delete": { "collection": TABLE, "id": "i1" } },
            { "UpdateWhere": {
                "collection": TABLE,
                "filters": [{ "field": "name", "operator": "gt", "column": "kind" }],
                "data": { "name": "Z" },
            } },
        ] }),
    )
    .await
    .expect_err("the column filter is invalid here");
    assert_eq!(err.code, ErrorCode::InvalidArgument, "{}", err.message);
    assert_eq!(count(&svc).await, 3, "nothing was written");
    svc.get(TABLE, "i1").await.expect("i1 was not deleted");
}

/// A batch whose last statement fails at the database (a duplicate primary
/// key) rolls back the writes before it.
#[tokio::test]
async fn a_failing_statement_rolls_the_whole_batch_back() {
    let svc = seeded().await;
    let err = dispatch(
        &svc,
        ServiceOp::DATABASE_BATCH,
        &serde_json::json!({ "ops": [
            { "Create": { "collection": TABLE, "data": { "id": "i4" } } },
            { "Update": { "collection": TABLE, "id": "i1", "data": { "name": "changed" } } },
            { "Create": { "collection": TABLE, "data": { "id": "i3" } } },
        ] }),
    )
    .await
    .expect_err("the duplicate key fails the batch");
    assert_eq!(err.code, ErrorCode::AlreadyExists, "{}", err.message);
    assert_eq!(count(&svc).await, 3, "i4 was rolled back");
    assert_eq!(
        svc.get(TABLE, "i1").await.expect("i1").data["name"],
        serde_json::json!("orig"),
        "the update was rolled back"
    );
}

/// A call carrying more than `MAX_BATCH_WRITES` rows or ops is refused as
/// `InvalidArgument` before any write; a call of exactly the limit runs.
#[tokio::test]
async fn calls_over_the_write_limit_are_refused_and_the_limit_itself_runs() {
    let svc = seeded().await;
    let rows = |n: usize| -> Vec<serde_json::Value> {
        (0..n)
            .map(|i| serde_json::json!({ "name": format!("r{i}") }))
            .collect()
    };
    let creates = |n: usize| -> Vec<serde_json::Value> {
        (0..n)
            .map(|i| {
                serde_json::json!({ "Create": {
                    "collection": TABLE, "data": { "name": format!("b{i}") },
                } })
            })
            .collect()
    };

    for (op, request) in [
        (
            ServiceOp::DATABASE_CREATE_MANY,
            serde_json::json!({ "collection": TABLE, "rows": rows(MAX_BATCH_WRITES + 1) }),
        ),
        (
            ServiceOp::DATABASE_BATCH,
            serde_json::json!({ "ops": creates(MAX_BATCH_WRITES + 1) }),
        ),
    ] {
        let err = dispatch(&svc, op, &request)
            .await
            .expect_err("one over the limit is refused");
        assert_eq!(
            err.code,
            ErrorCode::InvalidArgument,
            "{op}: {}",
            err.message
        );
        assert_eq!(count(&svc).await, 3, "{op}: nothing was written");
    }

    let resp = dispatch(
        &svc,
        ServiceOp::DATABASE_CREATE_MANY,
        &serde_json::json!({ "collection": TABLE, "rows": rows(MAX_BATCH_WRITES) }),
    )
    .await
    .expect("create_many of exactly the limit");
    assert_eq!(resp["rows_affected"], MAX_BATCH_WRITES);
    let resp = dispatch(
        &svc,
        ServiceOp::DATABASE_BATCH,
        &serde_json::json!({ "ops": creates(MAX_BATCH_WRITES) }),
    )
    .await
    .expect("batch of exactly the limit");
    assert_eq!(
        resp["results"].as_array().map(Vec::len),
        Some(MAX_BATCH_WRITES)
    );
    assert_eq!(count(&svc).await, 3 + 2 * MAX_BATCH_WRITES as i64);
}
