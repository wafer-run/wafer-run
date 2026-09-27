//! Statement counts, driven end to end: a typed client call → the context's
//! block call → the shared database handler → a real SQLite service whose
//! connection traces every statement it runs.
//!
//! On Cloudflare D1 every statement counts against the per-invocation budget
//! and is billed, and one sent on its own is a network round trip, so these
//! pin how many statements an operation costs once the schema is warm.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use wafer_block::{
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    types::{ResourceAccess, ResourceType},
    ErrorCode, Message, WaferError,
};
use wafer_core::{
    clients::database as db,
    interfaces::database::{
        handler::handle_message,
        service::{pk, Column, DataType, DatabaseService, Table},
    },
};

use crate::service::SQLiteDatabaseService;

/// Every statement a traced service sent SQLite, in order.
static STATEMENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Held for the whole of each test here: the trace log is process-wide, so
/// two traced tests running at once would read each other's statements.
static TRACED: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

/// An in-memory service whose connection records every statement.
fn traced_service() -> SQLiteDatabaseService {
    let mut conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
    conn.trace(Some(trace));
    SQLiteDatabaseService::new(conn)
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

/// `get_by_field` is the `SELECT … LIMIT 1` alone: it reads no total, so it
/// sends no `COUNT(*)`.
#[tokio::test]
async fn a_warm_get_by_field_runs_one_select_and_no_count() {
    let _traced = TRACED.lock().await;
    let svc = traced_service();
    svc.ensure_schema_table(&Table {
        name: "accounts".to_string(),
        columns: vec![pk("id"), Column::new("email", DataType::String)],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    })
    .await
    .expect("create accounts");
    for (id, email) in [("a1", "ada@example.com"), ("a2", "bob@example.com")] {
        svc.create(
            "accounts",
            HashMap::from([
                ("id".to_string(), serde_json::json!(id)),
                ("email".to_string(), serde_json::json!(email)),
            ]),
        )
        .await
        .expect("seed accounts");
    }
    let ctx = DbCtx { svc: Arc::new(svc) };

    // The first lookup warms the schema cache (table presence, columns).
    let found = db::get_by_field(
        &ctx,
        "accounts",
        "email",
        serde_json::json!("bob@example.com"),
    )
    .await
    .expect("cold lookup");
    assert_eq!(found.id, "a2");
    drain_statements();

    let found = db::get_by_field(
        &ctx,
        "accounts",
        "email",
        serde_json::json!("ada@example.com"),
    )
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
    let err = db::get_by_field(
        &ctx,
        "accounts",
        "email",
        serde_json::json!("eve@example.com"),
    )
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
