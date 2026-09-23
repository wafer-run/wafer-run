//! `database.insert_guarded` and `database.update_guarded` driven end to end:
//! request bytes encoded from a plain JSON-shaped value (what any peer puts on
//! the wire, independent of this build's Rust types) → the shared database
//! handler → the real SQLite service → the response decoded as plain JSON, so
//! the guard encoding and both response shapes are pinned too.

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

const TABLE: &str = "files";

async fn empty_files() -> SQLiteDatabaseService {
    let svc = SQLiteDatabaseService::open_in_memory().expect("open in-memory sqlite");
    svc.ensure_schema_table(&Table {
        name: TABLE.to_string(),
        columns: vec![
            pk("id"),
            Column::new("owner", DataType::Text).null(),
            Column::new("size", DataType::Int64).null(),
        ],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    })
    .await
    .expect("create files");
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

/// `owner`'s byte cap: `SUM(size) + add <= cap`, not counting `except`.
fn byte_cap(owner: &str, except: Option<&str>, add: i64, cap: i64) -> serde_json::Value {
    let mut filters =
        vec![serde_json::json!({ "field": "owner", "operator": "eq", "value": owner })];
    if let Some(id) = except {
        filters.push(serde_json::json!({ "field": "id", "operator": "neq", "value": id }));
    }
    serde_json::json!({ "SumAtMost": { "field": "size", "filters": filters, "add": add, "cap": cap } })
}

#[tokio::test]
async fn insert_guarded_returns_the_row_until_the_cap_then_the_refusing_guard() {
    let svc = empty_files().await;
    let guards = serde_json::json!([
        { "CountBelow": { "filters": [{ "field": "owner", "operator": "eq", "value": "u" }], "cap": 2 } }
    ]);
    let mut records = Vec::new();
    for i in 0..3 {
        let resp = dispatch(
            &svc,
            ServiceOp::DATABASE_INSERT_GUARDED,
            &serde_json::json!({
                "collection": TABLE,
                "data": { "id": format!("f{i}"), "owner": "u", "size": 1 },
                "guards": guards,
            }),
        )
        .await
        .expect("insert_guarded");
        records.push(resp);
    }
    assert_eq!(records[0]["Inserted"]["record"]["id"], "f0");
    assert_eq!(records[0]["Inserted"]["record"]["data"]["owner"], "u");
    assert_eq!(records[1]["Inserted"]["record"]["id"], "f1");
    assert_eq!(
        records[2],
        serde_json::json!({ "Refused": { "guard": 0 } }),
        "the third file is refused by guard 0"
    );
    assert_eq!(svc.count(TABLE, &[]).await.expect("count"), 2);
}

#[tokio::test]
async fn a_byte_cap_admits_landing_on_it_exactly() {
    let svc = empty_files().await;
    let mut admitted = Vec::new();
    for (id, size) in [("a", 70), ("b", 30), ("c", 1)] {
        let resp = dispatch(
            &svc,
            ServiceOp::DATABASE_INSERT_GUARDED,
            &serde_json::json!({
                "collection": TABLE,
                "data": { "id": id, "owner": "u", "size": size },
                "guards": [byte_cap("u", None, size, 100)],
            }),
        )
        .await
        .expect("insert_guarded");
        admitted.push(resp.get("Inserted").is_some());
    }
    assert_eq!(
        admitted,
        [true, true, false],
        "70 + 30 == 100 lands; + 1 does not"
    );
}

#[tokio::test]
async fn update_guarded_reports_the_rows_it_changed() {
    let svc = empty_files().await;
    for (id, size) in [("a", 70), ("b", 30)] {
        let data: HashMap<String, serde_json::Value> = [
            ("id", serde_json::json!(id)),
            ("owner", serde_json::json!("u")),
            ("size", serde_json::json!(size)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        svc.create(TABLE, data).await.expect("seed");
    }
    let replace_a = |size: i64| {
        serde_json::json!({
            "collection": TABLE,
            "filters": [{ "field": "id", "operator": "eq", "value": "a" }],
            "data": { "size": size },
            "guards": [byte_cap("u", Some("a"), size, 100)],
        })
    };
    let refused = dispatch(&svc, ServiceOp::DATABASE_UPDATE_GUARDED, &replace_a(71))
        .await
        .expect("update_guarded");
    assert_eq!(
        refused,
        serde_json::json!({ "Refused": { "guard": 0 } }),
        "30 + 71 > 100"
    );
    let landed = dispatch(&svc, ServiceOp::DATABASE_UPDATE_GUARDED, &replace_a(70))
        .await
        .expect("update_guarded");
    assert_eq!(
        landed,
        serde_json::json!({ "Updated": { "rows_affected": 1 } }),
        "30 + 70 == 100"
    );
    let gone = dispatch(
        &svc,
        ServiceOp::DATABASE_UPDATE_GUARDED,
        &serde_json::json!({
            "collection": TABLE,
            "filters": [{ "field": "id", "operator": "eq", "value": "gone" }],
            "data": { "size": 1 },
            "guards": [],
        }),
    )
    .await
    .expect("update_guarded");
    assert_eq!(gone, serde_json::json!("NoMatch"));
}

#[tokio::test]
async fn a_guard_naming_a_non_identifier_field_is_invalid_argument() {
    let svc = empty_files().await;
    let err = dispatch(
        &svc,
        ServiceOp::DATABASE_INSERT_GUARDED,
        &serde_json::json!({
            "collection": TABLE,
            "data": { "owner": "u", "size": 1 },
            "guards": [{ "SumAtMost": { "field": "size) --", "add": 1, "cap": 1 } }],
        }),
    )
    .await
    .expect_err("a hostile field is refused");
    assert_eq!(err.code, ErrorCode::InvalidArgument, "{}", err.message);
    assert_eq!(svc.count(TABLE, &[]).await.expect("count"), 0);
}

/// A taken key reaches the caller as `AlreadyExists` — from a guarded insert
/// and from a plain create — never as an internal error.
#[tokio::test]
async fn a_taken_key_is_already_exists_on_the_wire() {
    let svc = empty_files().await;
    let request = |op: &str| {
        let data = serde_json::json!({ "id": "a", "owner": "u", "size": 1 });
        if op == ServiceOp::DATABASE_INSERT_GUARDED {
            serde_json::json!({ "collection": TABLE, "data": data, "guards": [] })
        } else {
            serde_json::json!({ "collection": TABLE, "data": data })
        }
    };
    dispatch(
        &svc,
        ServiceOp::DATABASE_CREATE,
        &request(ServiceOp::DATABASE_CREATE),
    )
    .await
    .expect("first create");
    for op in [
        ServiceOp::DATABASE_INSERT_GUARDED,
        ServiceOp::DATABASE_CREATE,
    ] {
        let err = dispatch(&svc, op, &request(op))
            .await
            .expect_err("a duplicate id is refused");
        assert_eq!(err.code, ErrorCode::AlreadyExists, "{op}: {}", err.message);
    }
}
