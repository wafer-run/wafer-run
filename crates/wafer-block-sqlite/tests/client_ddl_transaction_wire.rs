//! `clients::database::ddl_transaction` driven end to end: the typed client →
//! the context's block call → the shared database handler → the real SQLite
//! service, and the answer back.
//!
//! A migration runner recognises another migrator's claim by the error a
//! failed statement answers with, so that error must reach the caller with
//! its classified code, the backend's message and the statement's position
//! intact. The other database ops scrub a duplicate key's message to a fixed
//! string at the handler; this one must not.

use std::sync::Arc;

use wafer_block::{
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    types::{ResourceAccess, ResourceType},
    wire::database::failed_statement_index,
    ErrorCode, Message, WaferError,
};
use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::{
    clients::database::{self as db, RawStatement},
    interfaces::database::{
        handler::handle_message,
        service::{pk, DatabaseService, Table},
    },
};

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

const LEDGER: &str = "applied";

fn raw(query: &str) -> RawStatement {
    RawStatement {
        query: query.to_string(),
        args: Vec::new(),
    }
}

/// A context over an empty `applied` table keyed by `id`.
async fn ctx() -> DbCtx {
    let svc = SQLiteDatabaseService::open_in_memory().expect("open in-memory sqlite");
    svc.ensure_schema_table(&Table {
        name: LEDGER.to_string(),
        columns: vec![pk("id")],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    })
    .await
    .expect("create the ledger");
    DbCtx { svc: Arc::new(svc) }
}

/// The claim pattern a migration runner uses: the ledger row is statement 0
/// and the file's DDL follows. A second run of the same claim is refused at
/// statement 0 as `AlreadyExists`, with the backend's message, and its DDL
/// never lands; the first run's commit is untouched.
#[tokio::test]
async fn a_taken_claim_answers_already_exists_at_statement_zero_with_the_backends_message() {
    let ctx = ctx().await;
    let file = [
        raw("INSERT INTO applied (id) VALUES ('001')"),
        raw("CREATE TABLE widgets (id TEXT PRIMARY KEY)"),
        raw("CREATE INDEX widgets_id ON widgets (id)"),
    ];

    let counts = db::ddl_transaction(&ctx, &file)
        .await
        .expect("the first run commits");
    assert_eq!(counts.len(), 3, "one count per statement: {counts:?}");
    assert_eq!(counts[0], 1, "the claim inserted one row");

    let err = db::ddl_transaction(&ctx, &file)
        .await
        .expect_err("the claim is taken");
    assert_eq!(err.code, ErrorCode::AlreadyExists, "{err:?}");
    assert_eq!(failed_statement_index(&err), Some(0), "{err:?}");
    assert!(
        err.message.contains("UNIQUE constraint failed: applied.id"),
        "the backend's message, not a fixed string: {err:?}"
    );
    assert!(
        ctx.svc
            .schema_table_exists("widgets")
            .await
            .expect("schema_table_exists"),
        "the first run's DDL stays committed"
    );
}

/// A statement after the claim that fails names itself: a runner can tell it
/// from a taken claim, and the claim it made rolls back with it.
#[tokio::test]
async fn a_failed_later_statement_names_its_index_and_rolls_back_the_claim() {
    let ctx = ctx().await;
    let err = db::ddl_transaction(
        &ctx,
        &[
            raw("INSERT INTO applied (id) VALUES ('002')"),
            raw("CREATE TABLE gadgets (id TEXT PRIMARY KEY)"),
            raw("ALTER TABLE no_such_table ADD COLUMN note TEXT"),
        ],
    )
    .await
    .expect_err("the ALTER names a missing table");
    assert_eq!(err.code, ErrorCode::Internal, "{err:?}");
    assert_eq!(failed_statement_index(&err), Some(2), "{err:?}");
    assert!(
        err.message.contains("no_such_table"),
        "the backend's message: {err:?}"
    );
    assert_eq!(
        ctx.svc.count(LEDGER, &[]).await.expect("count"),
        0,
        "the claim rolled back"
    );
    assert!(
        !ctx.svc
            .schema_table_exists("gadgets")
            .await
            .expect("schema_table_exists"),
        "the CREATE rolled back"
    );
}
