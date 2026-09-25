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
    ErrorCode, Message, WaferError,
};
use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::interfaces::database::{
    handler::handle_message,
    service::{pk, Column, DataType, DatabaseService, StatementBudget, Table},
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
    svc: &dyn DatabaseService,
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

async fn count(svc: &dyn DatabaseService) -> i64 {
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

/// The real SQLite service behind a budget it does not have: every op is
/// forwarded except `statement_budget`, which reports `budget`, as a backend
/// with a per-invocation limit (D1) would.
struct Budgeted {
    inner: SQLiteDatabaseService,
    budget: StatementBudget,
}

impl Budgeted {
    fn inner_service(&self) -> &dyn DatabaseService {
        &self.inner
    }
}

wafer_core::forward_database_service! {
    impl DatabaseService for Budgeted {
        forward_to inner_service();

        ops {
            get: forward,
            list: forward,
            create: forward,
            create_many: forward,
            update: forward,
            delete: forward,
            count: forward,
            sum: forward,
            query_raw: forward,
            exec_raw: forward,
            delete_where: forward,
            delete_where_count: forward,
            take_where: forward,
            update_where: forward,
            update_where_count: forward,
            increment_field_where: forward,
            upsert: forward,
            aggregate: forward,
            batch: forward,
            insert_guarded: forward,
            update_guarded: forward,
            ensure_schema_table: forward,
            ensure_schema_tables: forward,
            schema_table_exists: forward,
            schema_columns: forward,
            schema_drop_table: forward,
            schema_add_column: forward,
            set_strict_schema: forward,
            statement_budget: custom,
        }

        fn statement_budget(&self) -> StatementBudget {
            self.budget
        }
    }
}

async fn budgeted(limit: u64, used: u64) -> Budgeted {
    Budgeted {
        inner: seeded().await,
        budget: StatementBudget::Limited { limit, used },
    }
}

fn rows(n: usize) -> Vec<serde_json::Value> {
    (0..n)
        .map(|i| serde_json::json!({ "name": format!("r{i}") }))
        .collect()
}

fn creates(n: usize) -> Vec<serde_json::Value> {
    (0..n)
        .map(|i| {
            serde_json::json!({ "Create": {
                "collection": TABLE, "data": { "name": format!("b{i}") },
            } })
        })
        .collect()
}

/// A backend that runs at most N statements per invocation: a call of N + 1
/// rows or ops can never run there, so the handler refuses it as
/// `InvalidArgument` before any write; a call of exactly N runs.
#[tokio::test]
async fn a_call_over_the_backends_limit_is_refused_and_the_limit_itself_runs() {
    const LIMIT: usize = 20;
    for (op, over, at) in [
        (
            ServiceOp::DATABASE_CREATE_MANY,
            serde_json::json!({ "collection": TABLE, "rows": rows(LIMIT + 1) }),
            serde_json::json!({ "collection": TABLE, "rows": rows(LIMIT) }),
        ),
        (
            ServiceOp::DATABASE_BATCH,
            serde_json::json!({ "ops": creates(LIMIT + 1) }),
            serde_json::json!({ "ops": creates(LIMIT) }),
        ),
    ] {
        let svc = budgeted(LIMIT as u64, 0).await;
        let err = dispatch(&svc, op, &over)
            .await
            .expect_err("one over the limit is refused");
        assert_eq!(
            err.code,
            ErrorCode::InvalidArgument,
            "{op}: {}",
            err.message
        );
        assert!(
            err.message.contains(&format!("at most {LIMIT}")),
            "{op}: the refusal names the limit: {}",
            err.message
        );
        assert_eq!(count(&svc).await, 3, "{op}: nothing was written");

        dispatch(&svc, op, &at)
            .await
            .expect("a call of exactly the limit runs");
        assert_eq!(count(&svc).await, 3 + LIMIT as i64, "{op}");
    }
}

/// A backend that has already run statements in this invocation (as D1
/// counts them) refuses a call that fits its limit but not what is left, as
/// `ResourceExhausted`, before any write; a call that fits what is left runs.
#[tokio::test]
async fn a_call_over_what_the_invocation_has_left_is_exhausted() {
    for (op, over, fits) in [
        (
            ServiceOp::DATABASE_CREATE_MANY,
            serde_json::json!({ "collection": TABLE, "rows": rows(6) }),
            serde_json::json!({ "collection": TABLE, "rows": rows(5) }),
        ),
        (
            ServiceOp::DATABASE_BATCH,
            serde_json::json!({ "ops": creates(6) }),
            serde_json::json!({ "ops": creates(5) }),
        ),
    ] {
        let svc = budgeted(20, 15).await;
        let err = dispatch(&svc, op, &over)
            .await
            .expect_err("six statements with five left is refused");
        assert_eq!(
            err.code,
            ErrorCode::ResourceExhausted,
            "{op}: {}",
            err.message
        );
        assert!(
            err.message.contains("5 of its 20 left"),
            "{op}: the refusal names what is left and the limit: {}",
            err.message
        );
        assert_eq!(count(&svc).await, 3, "{op}: nothing was written");

        dispatch(&svc, op, &fits)
            .await
            .expect("a call that fits what is left runs");
        assert_eq!(count(&svc).await, 8, "{op}");
    }
}

/// Native SQLite has no per-invocation statement limit, so a call far past
/// the 1000 statements D1 allows one invocation runs whole.
#[tokio::test]
async fn sqlite_runs_a_call_of_any_size() {
    const LARGE: usize = 5000;
    let svc = seeded().await;
    assert_eq!(svc.statement_budget(), StatementBudget::Unbounded);
    let resp = dispatch(
        &svc,
        ServiceOp::DATABASE_CREATE_MANY,
        &serde_json::json!({ "collection": TABLE, "rows": rows(LARGE) }),
    )
    .await
    .expect("a large create_many");
    assert_eq!(resp["rows_affected"], LARGE);
    let resp = dispatch(
        &svc,
        ServiceOp::DATABASE_BATCH,
        &serde_json::json!({ "ops": creates(LARGE) }),
    )
    .await
    .expect("a large batch");
    assert_eq!(resp["results"].as_array().map(Vec::len), Some(LARGE));
    assert_eq!(count(&svc).await, 3 + 2 * LARGE as i64);
}

/// A `DeleteWhere` off the wire removes what its filters match and reports
/// the count as `DeletedWhere`; a create after it may reuse a removed row's
/// key, because it ran first in the same transaction.
#[tokio::test]
async fn batch_delete_where_then_create_replaces_the_matched_rows() {
    let svc = seeded().await;
    let resp = dispatch(
        &svc,
        ServiceOp::DATABASE_BATCH,
        &serde_json::json!({ "ops": [
            { "DeleteWhere": {
                "collection": TABLE,
                "filters": [{ "field": "kind", "value": "a" }],
            } },
            { "Create": { "collection": TABLE, "data": { "id": "i1", "name": "new", "kind": "a" } } },
        ] }),
    )
    .await
    .expect("batch");
    assert_eq!(
        resp["results"][0],
        serde_json::json!({ "DeletedWhere": { "rows_affected": 2 } }),
        "{resp}"
    );
    assert_eq!(resp["results"][1]["Created"]["id"], "i1", "{resp}");
    assert_eq!(count(&svc).await, 2, "i1 (replaced) and i3");
    assert_eq!(
        svc.get(TABLE, "i1").await.expect("i1").data["name"],
        serde_json::json!("new")
    );
}

/// The replace-a-table shape — an unfiltered `DeleteWhere`, then the new
/// rows — whose last create fails at the database leaves the table as it
/// was: the delete is rolled back with the creates.
#[tokio::test]
async fn a_failed_replacement_leaves_the_original_rows() {
    let svc = seeded().await;
    let err = dispatch(
        &svc,
        ServiceOp::DATABASE_BATCH,
        &serde_json::json!({ "ops": [
            { "DeleteWhere": { "collection": TABLE, "filters": [] } },
            { "Create": { "collection": TABLE, "data": { "id": "n1" } } },
            { "Create": { "collection": TABLE, "data": { "id": "n1" } } },
        ] }),
    )
    .await
    .expect_err("the duplicate key fails the replacement");
    assert_eq!(err.code, ErrorCode::AlreadyExists, "{}", err.message);
    assert_eq!(count(&svc).await, 3, "i1 i2 i3 survive, n1 was rolled back");
    for id in ["i1", "i2", "i3"] {
        svc.get(TABLE, id).await.expect("original row survives");
    }
}

/// A `DeleteWhere` is one statement, so it counts as one against the
/// statement budget however many rows it matches: with `LIMIT - 1` creates it
/// runs, with `LIMIT` creates the call is refused before anything is deleted.
#[tokio::test]
async fn a_delete_where_counts_as_one_statement_against_the_budget() {
    const LIMIT: usize = 20;
    let svc = budgeted(LIMIT as u64, 0).await;
    let ops = |creates: usize| -> Vec<serde_json::Value> {
        std::iter::once(serde_json::json!({ "DeleteWhere": {
            "collection": TABLE, "filters": [],
        } }))
        .chain((0..creates).map(|i| {
            serde_json::json!({ "Create": {
                "collection": TABLE, "data": { "name": format!("b{i}") },
            } })
        }))
        .collect()
    };

    let err = dispatch(
        &svc,
        ServiceOp::DATABASE_BATCH,
        &serde_json::json!({ "ops": ops(LIMIT) }),
    )
    .await
    .expect_err("one over the limit is refused");
    assert_eq!(err.code, ErrorCode::InvalidArgument, "{}", err.message);
    assert_eq!(count(&svc).await, 3, "nothing was deleted");

    let resp = dispatch(
        &svc,
        ServiceOp::DATABASE_BATCH,
        &serde_json::json!({ "ops": ops(LIMIT - 1) }),
    )
    .await
    .expect("exactly the limit");
    assert_eq!(
        resp["results"][0],
        serde_json::json!({ "DeletedWhere": { "rows_affected": 3 } })
    );
    assert_eq!(count(&svc).await, LIMIT as i64 - 1);
}
