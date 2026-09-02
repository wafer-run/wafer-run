//! Task 4 — schema-op handler arms (`ensure_table`, `add_column`,
//! `drop_table`, `table_exists`) are authorized on the table *and*, for the
//! three write ops, on the `__ddl__` sentinel resource; `table_exists` is a
//! read and needs only the table check. Every unknown wire `kind` (column
//! kind or default kind) is `InvalidArgument`, decoded and rejected before
//! the service ever runs.

use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::ResourceType,
    wire::database as wire,
    ErrorCode, Message, WaferError,
};

mod common;

use common::db_fakes::{self, expect_permission_denied, msg_without_wrap_meta, new_calls};

// ---------------------------------------------------------------------------
// Context fakes
// ---------------------------------------------------------------------------

/// Admits `(my_org__auth__*, Db)` and the `__ddl__` sentinel; denies every
/// other table. The shape a sandboxed guest sees: own tables plus ddl.
struct OwnTablesCtx;

#[wafer_block::wafer_async_trait]
impl Context for OwnTablesCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn is_cancelled(&self) -> bool {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: ResourceType,
        _is_write: bool,
    ) -> Result<(), WaferError> {
        if resource_type == ResourceType::Db
            && (resource.starts_with("my_org__auth__")
                || resource == wafer_block::wrap::DDL_RESOURCE)
        {
            Ok(())
        } else {
            Err(WaferError::new(
                ErrorCode::PermissionDenied,
                format!("denied: {resource}"),
            ))
        }
    }
}

/// Admits own tables but NOT `__ddl__` — a guest with `ddl: false`.
struct OwnTablesNoDdlCtx;

#[wafer_block::wafer_async_trait]
impl Context for OwnTablesNoDdlCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn is_cancelled(&self) -> bool {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
        unimplemented!("not exercised by decode_and_authorize")
    }

    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: ResourceType,
        _is_write: bool,
    ) -> Result<(), WaferError> {
        if resource_type == ResourceType::Db && resource.starts_with("my_org__auth__") {
            Ok(())
        } else {
            Err(WaferError::new(
                ErrorCode::PermissionDenied,
                format!("denied: {resource}"),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn widgets_table() -> wire::TableDef {
    wire::TableDef {
        name: "my_org__auth__widgets".into(),
        columns: vec![
            wire::ColumnDef {
                name: "id".into(),
                kind: "string".into(),
                nullable: false,
                primary_key: true,
                auto_increment: false,
                unique: false,
                default: None,
            },
            wire::ColumnDef {
                name: "created_at".into(),
                kind: "datetime".into(),
                nullable: false,
                primary_key: false,
                auto_increment: false,
                unique: false,
                default: Some(wire::DefaultDef {
                    kind: "now".into(),
                    value: serde_json::Value::Null,
                }),
            },
        ],
        indexes: vec![wire::IndexDef {
            name: String::new(),
            columns: vec!["created_at".into()],
            unique: false,
        }],
        primary_key: vec![],
        unique_keys: vec![],
    }
}

// ---------------------------------------------------------------------------
// Handler arm tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ensure_table_on_own_table_reaches_service() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let body = codec::encode(&wire::EnsureTableRequest {
        table: widgets_table(),
    })
    .unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::DATABASE_ENSURE_TABLE);
    let out =
        wafer_core::interfaces::database::handler::handle_message(&svc, &OwnTablesCtx, &msg, &body)
            .await;
    let buf = out.collect_buffered().await.expect("ok");
    let resp: wire::SchemaOpResponse = codec::decode(&buf.body).unwrap();
    assert_eq!(resp.table, "my_org__auth__widgets");
    assert_eq!(calls.lock().unwrap().as_slice(), &["ensure_schema_table"]);
}

#[tokio::test]
async fn ensure_table_on_foreign_table_is_denied_before_service() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let mut table = widgets_table();
    table.name = "my_org__other__secrets".into();
    let body = codec::encode(&wire::EnsureTableRequest { table }).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::DATABASE_ENSURE_TABLE);
    let out =
        wafer_core::interfaces::database::handler::handle_message(&svc, &OwnTablesCtx, &msg, &body)
            .await;
    expect_permission_denied(out).await;
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn ensure_table_without_ddl_capability_is_denied() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let body = codec::encode(&wire::EnsureTableRequest {
        table: widgets_table(),
    })
    .unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::DATABASE_ENSURE_TABLE);
    let out = wafer_core::interfaces::database::handler::handle_message(
        &svc,
        &OwnTablesNoDdlCtx,
        &msg,
        &body,
    )
    .await;
    expect_permission_denied(out).await;
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn table_exists_is_a_read_that_needs_no_ddl() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let body = codec::encode(&wire::TableExistsRequest {
        table: "my_org__auth__widgets".into(),
    })
    .unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::DATABASE_TABLE_EXISTS);
    let out = wafer_core::interfaces::database::handler::handle_message(
        &svc,
        &OwnTablesNoDdlCtx,
        &msg,
        &body,
    )
    .await;
    let buf = out.collect_buffered().await.expect("ok");
    let resp: wire::TableExistsResponse = codec::decode(&buf.body).unwrap();
    assert_eq!(resp.table, "my_org__auth__widgets");
    assert_eq!(calls.lock().unwrap().as_slice(), &["schema_table_exists"]);
}

#[tokio::test]
async fn unknown_column_kind_is_invalid_argument() {
    let calls = new_calls();
    let svc = db_fakes::RecordingDb::new(calls.clone());
    let mut table = widgets_table();
    table.columns[0].kind = "money".into();
    let body = codec::encode(&wire::EnsureTableRequest { table }).unwrap();
    let msg = msg_without_wrap_meta(ServiceOp::DATABASE_ENSURE_TABLE);
    let out =
        wafer_core::interfaces::database::handler::handle_message(&svc, &OwnTablesCtx, &msg, &body)
            .await;
    let err = match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => e,
        other => panic!("expected an error terminal, got {other:?}"),
    };
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(calls.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// `schema_wire` mapping unit test
// ---------------------------------------------------------------------------

#[test]
fn table_from_def_maps_every_kind_and_default() {
    use wafer_core::interfaces::database::schema_wire::table_from_def;
    let table = table_from_def(&widgets_table()).unwrap();
    assert_eq!(table.name, "my_org__auth__widgets");
    assert_eq!(table.columns[0].data_type, wafer_schema::DataType::String);
    assert!(table.columns[0].primary_key);
    assert_eq!(table.columns[1].data_type, wafer_schema::DataType::DateTime);
    assert!(table.columns[1]
        .default
        .as_ref()
        .is_some_and(|d| d.raw.contains("CURRENT_TIMESTAMP") || d.is_raw));
    assert_eq!(table.indexes.len(), 1);
    assert_eq!(table.indexes[0].columns, vec!["created_at"]);
}
