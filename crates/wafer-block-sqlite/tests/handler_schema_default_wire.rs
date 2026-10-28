//! A table or column `database.ensure_table` and `database.add_column`
//! cannot take, sent through the shared database handler to the real SQLite
//! service — a column default no SQL literal can express, a table without
//! the `updated_at` every write stamps: each answers `InvalidArgument`, and
//! the schema is left unchanged.

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
use wafer_core::interfaces::database::{handler::handle_message, service::DatabaseService};

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

const TABLE: &str = "widgets";

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

fn column(name: &str, default: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "kind": "text",
        "nullable": true,
        "primary_key": false,
        "auto_increment": false,
        "unique": false,
        "default": default.map(|v| serde_json::json!({ "kind": "value", "value": v })),
    })
}

fn id_column() -> serde_json::Value {
    serde_json::json!({
        "name": "id",
        "kind": "string",
        "nullable": false,
        "primary_key": true,
        "auto_increment": false,
        "unique": false,
        "default": null,
    })
}

fn ensure_table(columns: &[serde_json::Value]) -> serde_json::Value {
    serde_json::json!({
        "table": {
            "name": TABLE,
            "columns": columns,
            "indexes": [],
            "primary_key": [],
            "unique_keys": [],
        }
    })
}

#[tokio::test]
async fn ensure_table_with_a_nul_string_default_is_invalid_argument() {
    let svc = SQLiteDatabaseService::open_in_memory().expect("open in-memory sqlite");
    let err = dispatch(
        &svc,
        ServiceOp::DATABASE_ENSURE_TABLE,
        &ensure_table(&[
            id_column(),
            column("label", Some("a\0b")),
            column("updated_at", None),
        ]),
    )
    .await
    .expect_err("a NUL string default has no SQL literal");
    assert_eq!(err.code, ErrorCode::InvalidArgument, "{}", err.message);
    assert!(
        !svc.schema_table_exists(TABLE).await.expect("table_exists"),
        "the refused table must not be created"
    );
}

#[tokio::test]
async fn add_column_with_a_nul_string_default_is_invalid_argument() {
    let svc = SQLiteDatabaseService::open_in_memory().expect("open in-memory sqlite");
    dispatch(
        &svc,
        ServiceOp::DATABASE_ENSURE_TABLE,
        &ensure_table(&[id_column(), column("updated_at", None)]),
    )
    .await
    .expect("create widgets");
    let err = dispatch(
        &svc,
        ServiceOp::DATABASE_ADD_COLUMN,
        &serde_json::json!({ "table": TABLE, "column": column("label", Some("a\0b")) }),
    )
    .await
    .expect_err("a NUL string default has no SQL literal");
    assert_eq!(err.code, ErrorCode::InvalidArgument, "{}", err.message);
    assert_eq!(
        svc.schema_columns(TABLE).await.expect("columns"),
        vec!["id".to_string(), "updated_at".to_string()],
        "the refused column must not be added"
    );
}

/// A guest's `database.ensure_table` for a table without `updated_at` — the
/// column every typed write stamps — is refused by the host, naming the
/// table and the column, and nothing is created: the guest's `init` fails,
/// rather than every later write failing on the missing column. The guest
/// SDK may refuse it first; the host refuses it for every guest regardless.
#[tokio::test]
async fn ensure_table_without_updated_at_is_invalid_argument_naming_it() {
    let svc = SQLiteDatabaseService::open_in_memory().expect("open in-memory sqlite");
    let err = dispatch(
        &svc,
        ServiceOp::DATABASE_ENSURE_TABLE,
        &ensure_table(&[id_column(), column("created_at", None)]),
    )
    .await
    .expect_err("a table without updated_at is refused");
    assert_eq!(err.code, ErrorCode::InvalidArgument, "{}", err.message);
    assert!(
        err.message.contains(&format!("`{TABLE}`")) && err.message.contains("`updated_at`"),
        "the refusal names the table and the column: {}",
        err.message
    );
    assert!(
        !svc.schema_table_exists(TABLE).await.expect("table_exists"),
        "the refused table must not be created"
    );

    // Without `created_at`, which only a create stamps, the table is taken.
    dispatch(
        &svc,
        ServiceOp::DATABASE_ENSURE_TABLE,
        &ensure_table(&[id_column(), column("updated_at", None)]),
    )
    .await
    .expect("a table with updated_at and no created_at is created");
}
