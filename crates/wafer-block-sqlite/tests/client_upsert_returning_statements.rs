//! A windowed counter is incremented and read in ONE statement, driven end
//! to end: the typed client (`upsert_returning`) → the context's block call
//! → the shared database handler → the real SQLite service, with SQLite's own
//! statement trace counting what reaches the database.
//!
//! A rate limiter needs the counter's value after its increment. With only a
//! count of affected rows back from the upsert it had to read the row again:
//! two statements per check, and on Cloudflare D1 two round trips.

use std::sync::{Arc, Mutex};

use wafer_block::{
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    types::{ResourceAccess, ResourceType},
    wire::database::OnConflict,
    Message, WaferError,
};
use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::{
    clients::database as db,
    interfaces::database::{
        handler::handle_message,
        service::{pk, Column, DataType, DatabaseService, Table},
    },
};

/// Every statement the service sent SQLite, as its trace reported it. One
/// test in this binary, so nothing else writes here.
static STATEMENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// SQLite also traces the statements it runs inside one (a `pragma_*`
/// table-valued function's `PRAGMA`), prefixed `-- `; those are not
/// statements the service sent, so they are not counted.
fn trace(sql: &str) {
    if !sql.starts_with("-- ") {
        STATEMENTS.lock().unwrap().push(sql.to_string());
    }
}

/// Take the statements traced so far, leaving the log empty.
fn drain_statements() -> Vec<String> {
    std::mem::take(&mut *STATEMENTS.lock().unwrap())
}

/// A `Context` whose block calls land on the database handler over `svc`,
/// granting every resource, so a typed client call takes the path a block's
/// call takes.
struct DbCtx {
    svc: Arc<SQLiteDatabaseService>,
}

#[wafer_block::wafer_async_trait]
impl Context for DbCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        msg: Message,
        input: InputStream,
    ) -> OutputStream {
        let body = match input.collect_to_bytes().await {
            Ok(body) => body,
            Err(e) => return OutputStream::error(e),
        };
        handle_message(self.svc.as_ref(), self, &msg, &body).await
    }
    fn is_cancelled(&self) -> bool {
        false
    }
    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }
    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(DbCtx {
            svc: Arc::clone(&self.svc),
        })
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

const TABLE: &str = "rate_limits";

/// One rate-limiter check: count a request for `key` at `now` in a 60 s
/// window and answer the counter afterwards.
async fn hit(ctx: &DbCtx, id: &str, key: &str, now: i64) -> i64 {
    let row = db::upsert_returning(
        ctx,
        TABLE,
        vec![
            ("id".to_string(), serde_json::json!(id)),
            ("key".to_string(), serde_json::json!(key)),
        ],
        vec!["key".to_string()],
        OnConflict::WindowedCounter {
            count_field: "count".to_string(),
            window_field: "window_start".to_string(),
            now,
            window_cutoff: now - 60,
            created_fields: vec!["created_at".to_string()],
            updated_fields: vec!["updated_at".to_string()],
        },
    )
    .await
    .expect("windowed upsert")
    .expect("a counter upsert always answers its row");
    row.data["count"].as_i64().expect("count is an integer")
}

#[tokio::test]
async fn a_counter_increment_and_its_new_value_are_one_statement() {
    let mut conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
    conn.trace(Some(trace));
    let svc = SQLiteDatabaseService::new(conn);
    let mut key = Column::new("key", DataType::String);
    key.unique = true;
    svc.ensure_schema_table(&Table {
        name: TABLE.to_string(),
        columns: vec![
            pk("id"),
            key,
            Column::new("count", DataType::Int),
            Column::new("window_start", DataType::Int64),
            Column::new("created_at", DataType::Text),
            Column::new("updated_at", DataType::Text),
        ],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    })
    .await
    .expect("create rate_limits");
    // Migrations are authoritative in a deployment (D1 included).
    svc.set_strict_schema(true);
    let ctx = DbCtx { svc: Arc::new(svc) };

    let t0 = 1_700_000_000;
    // The first call reads the table's columns once, for the schema cache.
    assert_eq!(hit(&ctx, "r1", "login:ada", t0).await, 1);
    drain_statements();

    for (n, expected) in [(1, 2), (2, 3), (3, 4)] {
        let count = hit(&ctx, &format!("r{}", n + 1), "login:ada", t0 + n).await;
        assert_eq!(count, expected, "the counter after call {}", n + 1);
        let statements = drain_statements();
        assert_eq!(
            statements.len(),
            1,
            "an increment and its new value are one statement: {statements:#?}"
        );
        assert!(
            statements[0].starts_with("INSERT") && statements[0].ends_with("RETURNING *"),
            "{statements:#?}"
        );
    }

    // Past the window the counter restarts, still in one statement.
    assert_eq!(hit(&ctx, "r9", "login:ada", t0 + 600).await, 1);
    assert_eq!(drain_statements().len(), 1);
}
