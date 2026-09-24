//! A busy database answers `Unavailable`, not `Internal`, end to end: another
//! connection holds the write lock on a real SQLite file, a `database.create`
//! request goes through the shared database handler to the real service, and
//! the error it gets back carries the transient code a caller — and the
//! runtime, for a block whose Init hit the fault — may retry on. The Init-time
//! schema migration (`handle_lifecycle`) answers the same way. Once the lock
//! is released, both succeed.

use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::{ResourceAccess, ResourceType},
    ErrorCode, LifecycleEvent, LifecycleType, Message, WaferError,
};
use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::interfaces::database::{
    handler::{handle_lifecycle, handle_message},
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

async fn create(svc: &SQLiteDatabaseService, id: &str) -> Result<(), WaferError> {
    let body = codec::encode(&serde_json::json!({
        "collection": "notes",
        "data": { "id": id, "body": "hi" },
    }))
    .expect("encode request");
    match handle_message(
        svc,
        &AllowCtx,
        &Message::new(ServiceOp::DATABASE_CREATE),
        &body,
    )
    .await
    .collect_buffered()
    .await
    {
        Ok(_) => Ok(()),
        Err(TerminalNotResponse::Error(e)) => Err(e),
        Err(_) => panic!("the handler ended the stream without a response or error"),
    }
}

/// Waits out the service's busy timeout (5 s) twice: SQLite reports
/// `SQLITE_BUSY` only after its busy handler gives up.
#[tokio::test]
async fn a_write_against_a_locked_database_is_unavailable_until_the_lock_is_released() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = std::env::temp_dir().join(format!(
        "wafer-sqlite-transient-{}-{nonce}.db",
        std::process::id()
    ));
    let svc = SQLiteDatabaseService::open(path.to_str().expect("utf-8 temp path"))
        .expect("open file-backed sqlite");
    svc.ensure_schema_table(&Table {
        name: "notes".to_string(),
        columns: vec![pk("id"), Column::new("body", DataType::Text).null()],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    })
    .await
    .expect("create notes");

    let other = rusqlite::Connection::open(&path).expect("open a second connection");
    other
        .execute_batch("BEGIN IMMEDIATE")
        .expect("take the write lock");

    let err = create(&svc, "n1")
        .await
        .expect_err("the write cannot take the lock");
    assert_eq!(
        err.code,
        ErrorCode::Unavailable,
        "a busy database is transient: {err:?}"
    );
    assert_eq!(err.message, "database temporarily unavailable");

    let init = LifecycleEvent {
        event_type: LifecycleType::Init,
        data: Vec::new(),
    };
    let tags = [Table {
        name: "tags".to_string(),
        columns: vec![pk("id")],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    }];
    let err = handle_lifecycle(&svc, &tags, false, &init)
        .await
        .expect_err("the Init migration cannot take the lock");
    assert_eq!(
        err.code,
        ErrorCode::Unavailable,
        "a busy database during Init is transient: {err:?}"
    );

    other.execute_batch("ROLLBACK").expect("release the lock");
    create(&svc, "n1")
        .await
        .expect("the same write succeeds once the lock is released");
    handle_lifecycle(&svc, &tags, false, &init)
        .await
        .expect("the same Init migration succeeds once the lock is released");

    drop(svc);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
