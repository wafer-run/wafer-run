//! `clients::database::get_by_field` is one statement once the schema is
//! warm, driven end to end: the typed client → the context's block call →
//! the shared database handler → the real SQLite service, with SQLite's own
//! statement trace counting what reaches the database.
//!
//! On Cloudflare D1 every statement counts against the per-invocation budget
//! and is billed, so a lookup that also ran a `COUNT(*)` it never read paid
//! for a second statement on every call.

use std::sync::{Arc, Mutex};

use wafer_block::{
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    types::{ResourceAccess, ResourceType},
    ErrorCode, Message, WaferError,
};
use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::{
    clients::database as db,
    interfaces::database::{
        handler::handle_message,
        service::{pk, Column, DataType, DatabaseService, Table},
    },
};

/// Every statement SQLite ran, as its trace reported it. One test in this
/// binary, so nothing else writes here.
static STATEMENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn trace(sql: &str) {
    STATEMENTS.lock().unwrap().push(sql.to_string());
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

const TABLE: &str = "accounts";

#[tokio::test]
async fn a_warm_get_by_field_runs_one_select_and_no_count() {
    let mut conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
    conn.trace(Some(trace));
    let svc = SQLiteDatabaseService::new(conn);
    svc.ensure_schema_table(&Table {
        name: TABLE.to_string(),
        columns: vec![pk("id"), Column::new("email", DataType::String)],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    })
    .await
    .expect("create accounts");
    for (id, email) in [("a1", "ada@example.com"), ("a2", "bob@example.com")] {
        svc.create(
            TABLE,
            std::collections::HashMap::from([
                ("id".to_string(), serde_json::json!(id)),
                ("email".to_string(), serde_json::json!(email)),
            ]),
        )
        .await
        .expect("seed accounts");
    }
    let ctx = DbCtx { svc: Arc::new(svc) };

    // The first lookup warms the schema cache (table presence, columns).
    let found = db::get_by_field(&ctx, TABLE, "email", serde_json::json!("bob@example.com"))
        .await
        .expect("cold lookup");
    assert_eq!(found.id, "a2");
    drain_statements();

    let found = db::get_by_field(&ctx, TABLE, "email", serde_json::json!("ada@example.com"))
        .await
        .expect("warm lookup");
    assert_eq!(found.id, "a1");
    let statements = drain_statements();
    assert_eq!(
        statements.len(),
        1,
        "a warm get_by_field is one statement: {statements:#?}"
    );
    assert!(
        statements[0].starts_with("SELECT") && !statements[0].contains("COUNT("),
        "the one statement is the row select: {statements:#?}"
    );

    // A miss is still NotFound, from the same single statement.
    let err = db::get_by_field(&ctx, TABLE, "email", serde_json::json!("eve@example.com"))
        .await
        .expect_err("no row matches");
    assert_eq!(err.code, ErrorCode::NotFound);
    let statements = drain_statements();
    assert_eq!(
        statements.len(),
        1,
        "a miss is one statement too: {statements:#?}"
    );
}
