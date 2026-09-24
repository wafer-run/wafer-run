//! The collection a database request is authorized on is the table it runs
//! on, and a read never reshapes a table — driven end to end: each caller
//! block sends the request bytes a `db::*` client sends through
//! `ctx.call_block` to the REAL `wafer-run/database` block (the shared
//! database handler over a real in-memory SQLite service), which authorizes
//! the caller through the REAL `RuntimeContext::check_resource_access`.
//!
//! `acme/a-b` is the hyphenated twin of `acme/ab`: WRAP Rule 3 reads
//! `acme__a-b__t` as owned by `acme/a-b`, while stripping the `-` names
//! `acme/ab`'s table `acme__ab__t`. The table is read host-side (bypassing
//! WRAP, as a test oracle) to prove refused requests changed nothing.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use serde_json::{json, Value};
use wafer_block::{
    codec,
    common::ServiceOp,
    core_types::{LifecycleEvent, Message, WaferError},
    db::ListOptions,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::{ResourceGrant, ResourceType},
    Block, BlockInfo, ErrorCode,
};
use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::{
    interfaces::database::service::{pk, Column, DataType, DatabaseService, Table},
    service_blocks::database::register_with_tables,
};
use wafer_run::{Context, Wafer};

/// Owns [`TABLE`].
const OWNER: &str = "acme/ab";
const TABLE: &str = "acme__ab__t";
/// `OWNER`'s hyphenated twin; holds no grant on [`TABLE`].
const TWIN: &str = "acme/a-b";
/// [`TABLE`] as `TWIN` spells a table of its own.
const TWIN_TABLE: &str = "acme__a-b__t";
/// Holds `ResourceGrant::read` on [`TABLE`] and nothing else.
const READER: &str = "acme/reader";
/// Holds `ResourceGrant::append` and `ResourceGrant::read` on [`TABLE`].
const APPENDER: &str = "acme/appender";

/// The owning block: declares the grants, handles nothing.
struct Owner;

#[async_trait]
impl Block for Owner {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(OWNER, "0.1.0", "test/iface@v1", "owns acme__ab__t").grants(vec![
            ResourceGrant::read(READER, TABLE).typed(ResourceType::Db),
            ResourceGrant::append(APPENDER, TABLE),
            ResourceGrant::read(APPENDER, TABLE).typed(ResourceType::Db),
        ])
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
    async fn handle(&self, _ctx: &dyn Context, _m: Message, _i: InputStream) -> OutputStream {
        OutputStream::error(WaferError::new(ErrorCode::Unimplemented, "not called"))
    }
}

/// A caller: forwards the database request it is handed to
/// `wafer-run/database` from its own context, so the database handler sees
/// this block as the caller — exactly what a block's `db::*` client call does.
struct Caller(&'static str);

#[async_trait]
impl Block for Caller {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.0, "0.1.0", "test/iface@v1", "calls the database")
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        ctx.call_block("wafer-run/database", Message::new(msg.kind), input)
            .await
    }
}

async fn build() -> (Arc<Wafer>, Arc<SQLiteDatabaseService>) {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");
    let sqlite = Arc::new(SQLiteDatabaseService::open_in_memory().expect("open sqlite"));
    sqlite
        .ensure_schema_table(&Table {
            name: TABLE.to_string(),
            columns: vec![
                pk("id"),
                Column::new("name", DataType::Text).null(),
                Column::new("created_at", DataType::Text).null(),
                Column::new("updated_at", DataType::Text).null(),
            ],
            indexes: Vec::new(),
            primary_key: Vec::new(),
            unique_keys: Vec::new(),
        })
        .await
        .expect("create the owner's table");
    let seed: HashMap<String, Value> = [("id", json!("seed")), ("name", json!("orig"))]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    sqlite.create(TABLE, seed).await.expect("seed a row");

    register_with_tables(&mut wafer, sqlite.clone(), vec![]).expect("register database");
    wafer
        .register_block(OWNER, Arc::new(Owner))
        .expect("register owner");
    for caller in [TWIN, READER, APPENDER] {
        wafer
            .register_block(caller, Arc::new(Caller(caller)))
            .expect("register caller");
    }
    wafer.seal().await.expect("seal");
    (Arc::new(wafer), sqlite)
}

/// Run `op` with `request` as `caller`: the decoded response, or the code the
/// database refused it with.
async fn call(wafer: &Wafer, caller: &str, op: &str, request: &Value) -> Result<Value, ErrorCode> {
    let body = codec::encode(request).expect("encode request");
    let out = wafer
        .run_block(caller, Message::new(op), InputStream::from_bytes(body))
        .await;
    match out.collect_buffered().await {
        Ok(resp) if resp.body.is_empty() => Ok(Value::Null),
        Ok(resp) => Ok(codec::decode(&resp.body).expect("decode response as plain JSON")),
        Err(TerminalNotResponse::Error(e)) => Err(e.code),
        Err(other) => panic!("{op}: no response or error: {other:?}"),
    }
}

/// The owner's table as the host sees it: its columns and every row's
/// `(id, name)`.
async fn snapshot(sqlite: &SQLiteDatabaseService) -> (Vec<String>, Vec<(String, Value)>) {
    let columns = sqlite.schema_columns(TABLE).await.expect("schema_columns");
    let mut rows: Vec<(String, Value)> = sqlite
        .list(TABLE, &ListOptions::default())
        .await
        .expect("list")
        .records
        .into_iter()
        .map(|r| (r.id, r.data.get("name").cloned().unwrap_or(Value::Null)))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    (columns, rows)
}

#[tokio::test]
async fn hyphenated_twin_is_refused_its_stripped_namesakes_table() {
    let (wafer, sqlite) = build().await;
    let before = snapshot(&sqlite).await;

    // The twin may not name the owner's table: Rule 3 refuses it.
    assert_eq!(
        call(
            &wafer,
            TWIN,
            ServiceOp::DATABASE_LIST,
            &json!({ "collection": TABLE })
        )
        .await,
        Err(ErrorCode::PermissionDenied),
        "{TWIN} listing {TABLE} directly"
    );

    // Its own spelling is not a table name, so it is refused before it is
    // authorized — it never reaches `acme__ab__t`.
    let data = json!({ "name": "twin" });
    let requests = [
        (
            "list",
            ServiceOp::DATABASE_LIST,
            json!({ "collection": TWIN_TABLE }),
        ),
        (
            "count",
            ServiceOp::DATABASE_COUNT,
            json!({ "collection": TWIN_TABLE }),
        ),
        (
            "get",
            ServiceOp::DATABASE_GET,
            json!({ "collection": TWIN_TABLE, "id": "seed" }),
        ),
        (
            "create",
            ServiceOp::DATABASE_CREATE,
            json!({ "collection": TWIN_TABLE, "data": data }),
        ),
        (
            "update",
            ServiceOp::DATABASE_UPDATE,
            json!({ "collection": TWIN_TABLE, "id": "seed", "data": data }),
        ),
        (
            "update_where",
            ServiceOp::DATABASE_UPDATE_WHERE,
            json!({ "collection": TWIN_TABLE, "filters": [], "data": data }),
        ),
        (
            "delete_where",
            ServiceOp::DATABASE_DELETE_WHERE,
            json!({ "collection": TWIN_TABLE, "filters": [] }),
        ),
        (
            "batch Create",
            ServiceOp::DATABASE_BATCH,
            json!({ "ops": [{ "Create": { "collection": TWIN_TABLE, "data": data } }] }),
        ),
    ];
    let mut wrong = Vec::new();
    for (label, op, request) in &requests {
        let got = call(&wafer, TWIN, op, request).await;
        if got != Err(ErrorCode::InvalidArgument) {
            wrong.push(format!("{label}: {got:?}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "{TWIN} spelling {TWIN_TABLE} must be InvalidArgument, got:\n{}",
        wrong.join("\n")
    );
    assert_eq!(
        snapshot(&sqlite).await,
        before,
        "the owner's table is untouched"
    );
}

#[tokio::test]
async fn read_only_caller_cannot_add_a_column_by_reading() {
    let (wafer, sqlite) = build().await;
    let before = snapshot(&sqlite).await;

    // The grant works: a read on known columns is answered.
    let listed = call(
        &wafer,
        READER,
        ServiceOp::DATABASE_LIST,
        &json!({ "collection": TABLE, "sort": [{ "field": "name" }] }),
    )
    .await
    .expect("a read-only caller lists the table");
    assert_eq!(listed["records"][0]["id"], "seed", "{listed}");

    let unknown = json!([{ "field": "zz", "value": "x" }]);
    let reads = [
        (
            "list sorted on zz",
            ServiceOp::DATABASE_LIST,
            json!({ "collection": TABLE, "sort": [{ "field": "zz" }] }),
        ),
        (
            "list filtered on zz",
            ServiceOp::DATABASE_LIST,
            json!({ "collection": TABLE, "filters": unknown }),
        ),
        (
            "count filtered on zz",
            ServiceOp::DATABASE_COUNT,
            json!({ "collection": TABLE, "filters": unknown }),
        ),
    ];
    let mut wrong = Vec::new();
    for (label, op, request) in &reads {
        let got = call(&wafer, READER, op, request).await;
        if got != Err(ErrorCode::InvalidArgument) {
            wrong.push(format!("{label}: {got:?}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "a read naming an unknown column must be InvalidArgument, got:\n{}",
        wrong.join("\n")
    );
    let after = snapshot(&sqlite).await;
    assert!(
        !after.0.contains(&"zz".to_string()),
        "a read-only caller added column zz: {:?}",
        after.0
    );
    assert_eq!(after, before, "the owner's table is untouched");
}

#[tokio::test]
async fn append_only_guard_on_a_new_column_is_refused_and_adds_nothing() {
    let (wafer, sqlite) = build().await;
    let before = snapshot(&sqlite).await;

    // The grant works: a guarded insert over known columns lands.
    let landed = call(
        &wafer,
        APPENDER,
        ServiceOp::DATABASE_INSERT_GUARDED,
        &json!({
            "collection": TABLE,
            "data": { "name": "guarded" },
            "guards": [{ "CountBelow": {
                "filters": [{ "field": "name", "value": "guarded" }], "cap": 5,
            } }],
        }),
    )
    .await
    .expect("an append-and-read caller inserts under a guard on known columns");
    assert_eq!(landed["Inserted"]["record"]["data"]["name"], "guarded", "{landed}");
    let with_insert = snapshot(&sqlite).await;
    assert_eq!(with_insert.0, before.0, "the insert added no column");

    let got = call(
        &wafer,
        APPENDER,
        ServiceOp::DATABASE_INSERT_GUARDED,
        &json!({
            "collection": TABLE,
            "data": { "name": "sneaky" },
            "guards": [{ "CountBelow": {
                "filters": [{ "field": "zz", "value": "x" }], "cap": 5,
            } }],
        }),
    )
    .await;
    assert_eq!(
        got,
        Err(ErrorCode::InvalidArgument),
        "a guard naming an unknown column"
    );
    assert_eq!(
        snapshot(&sqlite).await,
        with_insert,
        "the refused insert added no column and no row"
    );
}
