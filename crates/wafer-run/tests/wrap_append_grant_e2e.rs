//! Append-only WRAP grants, driven end to end: a grantee block sends the
//! request bytes a `database.*` client sends through `ctx.call_block` to the
//! REAL `wafer-run/database` block (the shared database handler over a real
//! in-memory SQLite service), which authorizes the grantee through the REAL
//! `RuntimeContext::check_resource_access` against the grant the owning
//! block declared in its `BlockInfo`.
//!
//! An append-only grantee must be able to insert (`create`, `create_many`, a
//! batch of `Create`s) and must get `PermissionDenied` from every op that
//! reads, changes, removes or reshapes the collection. The table is then
//! read host-side (bypassing WRAP, as a test oracle) to prove the denied ops
//! never ran: the seeded row is untouched and only the admitted inserts
//! landed.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use serde_json::{json, Value};
use wafer_block::{
    codec,
    common::ServiceOp,
    core_types::{LifecycleEvent, Message, WaferError},
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

const OWNER: &str = "test-org/ledger";
const AUDIT: &str = "test_org__ledger__audit";
/// A collection of the same owner with no `created_at` / `updated_at`.
const BARE: &str = "test_org__ledger__bare";
/// Holds `ResourceGrant::append` on [`AUDIT`] and nothing else.
const APPENDER: &str = "test-org/appender";
/// Holds `ResourceGrant::append` AND `ResourceGrant::read` on [`AUDIT`].
const READ_APPENDER: &str = "test-org/read-appender";
/// Holds `ResourceGrant::read_write` on [`AUDIT`]: the append-only insert
/// rules do not apply to it.
const WRITER: &str = "test-org/writer";

/// The owning block: declares the grants, handles nothing.
struct Ledger;

#[async_trait]
impl Block for Ledger {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(OWNER, "0.1.0", "test/iface@v1", "owns the audit table").grants(vec![
            ResourceGrant::append(APPENDER, AUDIT),
            ResourceGrant::append(APPENDER, BARE),
            ResourceGrant::append(READ_APPENDER, AUDIT),
            ResourceGrant::read(READ_APPENDER, AUDIT).typed(ResourceType::Db),
            ResourceGrant::read_write(WRITER, AUDIT).typed(ResourceType::Db),
        ])
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
    async fn handle(&self, _ctx: &dyn Context, _m: Message, _i: InputStream) -> OutputStream {
        OutputStream::error(WaferError::new(ErrorCode::Unimplemented, "not called"))
    }
}

/// A grantee: forwards the database request it is handed to
/// `wafer-run/database` from its own context, so the database handler sees
/// this block as the caller — exactly what a block's `db::*` client call does.
struct Grantee(&'static str);

#[async_trait]
impl Block for Grantee {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            self.0,
            "0.1.0",
            "test/iface@v1",
            "writes to the audit table",
        )
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
            name: AUDIT.to_string(),
            columns: vec![
                pk("id"),
                Column::new("action", DataType::Text).null(),
                Column::new("n", DataType::Int).null(),
            ],
            indexes: Vec::new(),
            primary_key: Vec::new(),
            unique_keys: Vec::new(),
        })
        .await
        .expect("create the audit table");
    let seed: HashMap<String, Value> = [
        ("id", json!("seed")),
        ("action", json!("orig")),
        ("n", json!(1)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    sqlite.create(AUDIT, seed).await.expect("seed a row");

    register_with_tables(&mut wafer, sqlite.clone(), vec![]).expect("register database");
    wafer
        .register_block(OWNER, Arc::new(Ledger))
        .expect("register owner");
    for grantee in [APPENDER, READ_APPENDER, WRITER] {
        wafer
            .register_block(grantee, Arc::new(Grantee(grantee)))
            .expect("register grantee");
    }
    wafer.seal().await.expect("seal");
    (Arc::new(wafer), sqlite)
}

/// Run `op` with `request` as `caller`; `Ok(())` when the database answered,
/// `Err(code)` when it refused.
async fn call(wafer: &Wafer, caller: &str, op: &str, request: &Value) -> Result<(), ErrorCode> {
    let body = codec::encode(request).expect("encode request");
    let out = wafer
        .run_block(caller, Message::new(op), InputStream::from_bytes(body))
        .await;
    match out.collect_buffered().await {
        Ok(_) => Ok(()),
        Err(TerminalNotResponse::Error(e)) => Err(e.code),
        Err(other) => panic!("{op}: no response or error: {other:?}"),
    }
}

fn seed_filter() -> Value {
    json!([{ "field": "id", "value": "seed" }])
}

/// Every op an append-only grantee must be refused, with a request that
/// would otherwise succeed against the seeded row.
fn refused_ops() -> Vec<(&'static str, &'static str, Value)> {
    let guard = json!([{ "CountBelow": { "filters": [], "cap": 1000 } }]);
    let upsert = json!({
        "collection": AUDIT,
        "data": [["id", "seed"], ["action", "overwritten"]],
        "conflict_columns": ["id"],
        "on_conflict": { "SetColumns": ["action"] },
    });
    let set = json!({ "action": "overwritten" });
    let batch = |write: Value| json!({ "ops": [write] });
    vec![
        // Reads: an append grant does not include read.
        (
            "get",
            ServiceOp::DATABASE_GET,
            json!({ "collection": AUDIT, "id": "seed" }),
        ),
        (
            "list",
            ServiceOp::DATABASE_LIST,
            json!({ "collection": AUDIT }),
        ),
        (
            "count",
            ServiceOp::DATABASE_COUNT,
            json!({ "collection": AUDIT }),
        ),
        (
            "sum",
            ServiceOp::DATABASE_SUM,
            json!({ "collection": AUDIT, "field": "n" }),
        ),
        (
            "aggregate",
            ServiceOp::DATABASE_AGGREGATE,
            json!({ "collection": AUDIT, "aggregates": [{ "Count": { "alias": "c" } }] }),
        ),
        (
            "table_exists",
            ServiceOp::DATABASE_TABLE_EXISTS,
            json!({ "table": AUDIT }),
        ),
        // An insert whose guards measure existing rows needs read too.
        (
            "insert_guarded",
            ServiceOp::DATABASE_INSERT_GUARDED,
            json!({ "collection": AUDIT, "data": { "action": "guarded" }, "guards": guard }),
        ),
        // Modifications.
        (
            "update",
            ServiceOp::DATABASE_UPDATE,
            json!({ "collection": AUDIT, "id": "seed", "data": set }),
        ),
        (
            "update_where",
            ServiceOp::DATABASE_UPDATE_WHERE,
            json!({ "collection": AUDIT, "filters": seed_filter(), "data": set }),
        ),
        (
            "update_where_count",
            ServiceOp::DATABASE_UPDATE_WHERE_COUNT,
            json!({ "collection": AUDIT, "filters": seed_filter(), "data": set }),
        ),
        (
            "update_guarded",
            ServiceOp::DATABASE_UPDATE_GUARDED,
            json!({ "collection": AUDIT, "filters": seed_filter(), "data": set, "guards": guard }),
        ),
        (
            "increment_field_where",
            ServiceOp::DATABASE_INCREMENT_FIELD_WHERE,
            json!({ "collection": AUDIT, "col": "n", "delta": 5, "filters": seed_filter() }),
        ),
        ("upsert", ServiceOp::DATABASE_UPSERT, upsert.clone()),
        (
            "delete",
            ServiceOp::DATABASE_DELETE,
            json!({ "collection": AUDIT, "id": "seed" }),
        ),
        (
            "delete_where",
            ServiceOp::DATABASE_DELETE_WHERE,
            json!({ "collection": AUDIT, "filters": seed_filter() }),
        ),
        (
            "delete_where_count",
            ServiceOp::DATABASE_DELETE_WHERE_COUNT,
            json!({ "collection": AUDIT, "filters": seed_filter() }),
        ),
        (
            "take_where",
            ServiceOp::DATABASE_TAKE_WHERE,
            json!({ "collection": AUDIT, "filters": seed_filter() }),
        ),
        // Batch writes other than `Create`, alone and behind an admitted
        // `Create` (the batch is refused whole, so the `Create` never lands).
        (
            "batch Update",
            ServiceOp::DATABASE_BATCH,
            batch(json!({ "Update": { "collection": AUDIT, "id": "seed", "data": set } })),
        ),
        (
            "batch Delete",
            ServiceOp::DATABASE_BATCH,
            batch(json!({ "Delete": { "collection": AUDIT, "id": "seed" } })),
        ),
        (
            "batch UpdateWhere",
            ServiceOp::DATABASE_BATCH,
            batch(json!({
                "UpdateWhere": { "collection": AUDIT, "filters": seed_filter(), "data": set }
            })),
        ),
        (
            "batch Upsert",
            ServiceOp::DATABASE_BATCH,
            batch(json!({ "Upsert": upsert })),
        ),
        (
            "batch Create + Delete",
            ServiceOp::DATABASE_BATCH,
            json!({ "ops": [
                { "Create": { "collection": AUDIT, "data": { "action": "smuggled" } } },
                { "Delete": { "collection": AUDIT, "id": "seed" } },
            ] }),
        ),
        // Schema ops on the collection.
        (
            "add_column",
            ServiceOp::DATABASE_ADD_COLUMN,
            json!({ "table": AUDIT, "column": { "name": "evil", "kind": "text", "nullable": true } }),
        ),
        (
            "drop_table",
            ServiceOp::DATABASE_DROP_TABLE,
            json!({ "table": AUDIT }),
        ),
        (
            "ensure_table",
            ServiceOp::DATABASE_ENSURE_TABLE,
            json!({ "table": { "name": AUDIT, "columns": [
                { "name": "id", "kind": "text", "primary_key": true },
            ] } }),
        ),
    ]
}

#[tokio::test]
async fn append_only_grantee_inserts_and_is_refused_everything_else() {
    let (wafer, sqlite) = build().await;
    let mut wrong: Vec<String> = Vec::new();

    let admitted = [
        (
            "create",
            ServiceOp::DATABASE_CREATE,
            json!({ "collection": AUDIT, "data": { "action": "created" } }),
        ),
        (
            "create_many",
            ServiceOp::DATABASE_CREATE_MANY,
            json!({ "collection": AUDIT, "rows": [{ "action": "many" }, { "action": "many" }] }),
        ),
        (
            "batch Create",
            ServiceOp::DATABASE_BATCH,
            json!({ "ops": [
                { "Create": { "collection": AUDIT, "data": { "action": "batched" } } },
            ] }),
        ),
    ];
    for (label, op, request) in &admitted {
        if let Err(code) = call(&wafer, APPENDER, op, request).await {
            wrong.push(format!("{label}: expected to be admitted, got {code:?}"));
        }
    }
    for (label, op, request) in refused_ops() {
        match call(&wafer, APPENDER, op, &request).await {
            Err(ErrorCode::PermissionDenied) => {}
            other => wrong.push(format!("{label}: expected PermissionDenied, got {other:?}")),
        }
    }
    assert!(
        wrong.is_empty(),
        "append-only grantee:\n  {}",
        wrong.join("\n  ")
    );

    // Oracle, bypassing WRAP: the seeded row is untouched, the table kept
    // its shape, and exactly the four admitted inserts landed.
    let seed = sqlite.get(AUDIT, "seed").await.expect("seed row survives");
    assert_eq!(seed.data.get("action"), Some(&json!("orig")));
    assert_eq!(seed.data.get("n"), Some(&json!(1)));
    assert!(!seed.data.contains_key("evil"), "add_column must not run");
    assert_eq!(sqlite.count(AUDIT, &[]).await.expect("count"), 5);
}

/// `insert_guarded` is refused to an append-only grantee because its guards
/// read existing rows; holding a read grant as well admits it. Pins that the
/// refusal above is the missing read, not a blanket refusal of the op.
#[tokio::test]
async fn insert_guarded_needs_read_alongside_append() {
    let (wafer, sqlite) = build().await;
    let request = json!({
        "collection": AUDIT,
        "data": { "action": "guarded" },
        "guards": [{ "CountBelow": { "filters": [], "cap": 1000 } }],
    });
    assert_eq!(
        call(
            &wafer,
            APPENDER,
            ServiceOp::DATABASE_INSERT_GUARDED,
            &request
        )
        .await,
        Err(ErrorCode::PermissionDenied)
    );
    assert_eq!(
        call(
            &wafer,
            READ_APPENDER,
            ServiceOp::DATABASE_INSERT_GUARDED,
            &request
        )
        .await,
        Ok(())
    );
    // Read + append still is not write.
    assert_eq!(
        call(
            &wafer,
            READ_APPENDER,
            ServiceOp::DATABASE_DELETE,
            &json!({ "collection": AUDIT, "id": "seed" }),
        )
        .await,
        Err(ErrorCode::PermissionDenied)
    );
    assert_eq!(sqlite.count(AUDIT, &[]).await.expect("count"), 2);
}

/// An append-only insert may not add a column: outside STRICT_SCHEMA the
/// service would ALTER an unseen column into the owner's table (typed by the
/// first value). Every insert path refuses it before the service runs; a
/// read-write grantee still grows the table as before.
#[tokio::test]
async fn append_only_inserts_cannot_add_columns() {
    let (wafer, sqlite) = build().await;
    let evil = json!({ "action": "a", "evil": 1 });
    let attempts = [
        (
            "create",
            APPENDER,
            ServiceOp::DATABASE_CREATE,
            json!({ "collection": AUDIT, "data": evil }),
        ),
        (
            "create_many",
            APPENDER,
            ServiceOp::DATABASE_CREATE_MANY,
            json!({ "collection": AUDIT, "rows": [{ "action": "ok" }, evil] }),
        ),
        (
            "batch Create",
            APPENDER,
            ServiceOp::DATABASE_BATCH,
            json!({ "ops": [{ "Create": { "collection": AUDIT, "data": evil } }] }),
        ),
        (
            "insert_guarded",
            READ_APPENDER,
            ServiceOp::DATABASE_INSERT_GUARDED,
            json!({
                "collection": AUDIT,
                "data": evil,
                "guards": [{ "CountBelow": { "filters": [], "cap": 1000 } }],
            }),
        ),
    ];
    let mut wrong = Vec::new();
    for (label, caller, op, request) in &attempts {
        match call(&wafer, caller, op, request).await {
            Err(ErrorCode::PermissionDenied) => {}
            other => wrong.push(format!("{label}: expected PermissionDenied, got {other:?}")),
        }
    }
    assert!(wrong.is_empty(), "unseen column:\n  {}", wrong.join("\n  "));
    let columns = sqlite.schema_columns(AUDIT).await.expect("columns");
    assert!(!columns.contains(&"evil".to_string()), "{columns:?}");
    assert_eq!(sqlite.count(AUDIT, &[]).await.expect("count"), 1);

    // A read-write grantee is not append-only: the column is added.
    let grown = json!({ "collection": AUDIT, "data": { "action": "w", "grown": 1 } });
    assert_eq!(
        call(&wafer, WRITER, ServiceOp::DATABASE_CREATE, &grown).await,
        Ok(())
    );
    let columns = sqlite.schema_columns(AUDIT).await.expect("columns");
    assert!(columns.contains(&"grown".to_string()), "{columns:?}");
}

/// An append-only insert may not choose a row's `id`, `created_at` or
/// `updated_at` — the server stamps them, so an append-only grantee cannot
/// forge or back-date an entry. The refusal covers every insert path and any
/// letter case; a read-write grantee still chooses them.
#[tokio::test]
async fn append_only_inserts_cannot_set_server_owned_columns() {
    let (wafer, sqlite) = build().await;
    let mut wrong = Vec::new();
    for (column, value) in [
        ("id", json!("forged")),
        ("ID", json!("forged")),
        ("created_at", json!("2000-01-01T00:00:00Z")),
        ("updated_at", json!("2000-01-01T00:00:00Z")),
    ] {
        let data = json!({ "action": "a", column: value });
        for (op, request) in [
            (
                ServiceOp::DATABASE_CREATE,
                json!({ "collection": AUDIT, "data": data }),
            ),
            (
                ServiceOp::DATABASE_CREATE_MANY,
                json!({ "collection": AUDIT, "rows": [data] }),
            ),
            (
                ServiceOp::DATABASE_BATCH,
                json!({ "ops": [{ "Create": { "collection": AUDIT, "data": data } }] }),
            ),
        ] {
            match call(&wafer, APPENDER, op, &request).await {
                Err(ErrorCode::PermissionDenied) => {}
                other => wrong.push(format!(
                    "{op} setting `{column}`: expected PermissionDenied, got {other:?}"
                )),
            }
        }
    }
    // `insert_guarded` needs read as well, so it goes through the grantee
    // holding both; it is still an append-only insert.
    for (column, value) in [
        ("id", json!("forged")),
        ("created_at", json!("2000-01-01T00:00:00Z")),
        ("updated_at", json!("2000-01-01T00:00:00Z")),
    ] {
        let request = json!({
            "collection": AUDIT,
            "data": { "action": "a", column: value },
            "guards": [{ "CountBelow": { "filters": [], "cap": 1000 } }],
        });
        match call(
            &wafer,
            READ_APPENDER,
            ServiceOp::DATABASE_INSERT_GUARDED,
            &request,
        )
        .await
        {
            Err(ErrorCode::PermissionDenied) => {}
            other => wrong.push(format!(
                "insert_guarded setting `{column}`: expected PermissionDenied, got {other:?}"
            )),
        }
    }
    assert!(
        wrong.is_empty(),
        "server-owned column:\n  {}",
        wrong.join("\n  ")
    );
    assert_eq!(sqlite.count(AUDIT, &[]).await.expect("count"), 1);
    assert!(sqlite.get(AUDIT, "forged").await.is_err());

    // What an append-only insert stores carries server-assigned values.
    assert_eq!(
        call(
            &wafer,
            APPENDER,
            ServiceOp::DATABASE_CREATE,
            &json!({ "collection": AUDIT, "data": { "action": "stamped" } }),
        )
        .await,
        Ok(())
    );
    let stamped: Vec<_> = sqlite
        .list(AUDIT, &wafer_block::db::ListOptions::default())
        .await
        .expect("list")
        .records
        .into_iter()
        .filter(|r| r.data.get("action") == Some(&json!("stamped")))
        .collect();
    assert_eq!(stamped.len(), 1);
    assert!(!stamped[0].id.is_empty());
    let created = stamped[0].data["created_at"].as_str().expect("created_at");
    assert!(!created.starts_with("2000"), "created_at: {created}");

    // A read-write grantee chooses them.
    let chosen = json!({
        "collection": AUDIT,
        "data": { "id": "chosen", "action": "w", "created_at": "2000-01-01T00:00:00Z" },
    });
    assert_eq!(
        call(&wafer, WRITER, ServiceOp::DATABASE_CREATE, &chosen).await,
        Ok(())
    );
    let row = sqlite.get(AUDIT, "chosen").await.expect("chosen row");
    assert_eq!(row.data["created_at"], json!("2000-01-01T00:00:00Z"));
}

/// A table missing any of `id`, `created_at`, `updated_at` refuses every
/// append-only insert: the server would stamp the missing column, and
/// outside STRICT_SCHEMA stamping it would add it to the table.
#[tokio::test]
async fn append_only_inserts_need_the_server_owned_columns() {
    let (wafer, sqlite) = build().await;
    sqlite
        .ensure_schema_table(&Table {
            name: BARE.to_string(),
            columns: vec![pk("id"), Column::new("action", DataType::Text).null()],
            indexes: Vec::new(),
            primary_key: Vec::new(),
            unique_keys: Vec::new(),
        })
        .await
        .expect("create a table without timestamps");
    let request = json!({ "collection": BARE, "data": { "action": "a" } });
    assert_eq!(
        call(&wafer, APPENDER, ServiceOp::DATABASE_CREATE, &request).await,
        Err(ErrorCode::PermissionDenied)
    );
    let columns = sqlite.schema_columns(BARE).await.expect("columns");
    assert!(!columns.contains(&"created_at".to_string()), "{columns:?}");
    assert_eq!(sqlite.count(BARE, &[]).await.expect("count"), 0);
}
