//! `clients::database::list_all` / `list_sorted` never return a silent
//! prefix, driven end to end: the typed client → the context's block call →
//! the shared database handler → the real SQLite service.
//!
//! A read that matches up to [`LIST_ALL_MAX_ROWS`] rows returns all of them;
//! one that matches a single row more is refused with `OutOfRange` instead of
//! being cut at the cap.

use std::{collections::HashMap, sync::Arc};

use wafer_block::{
    context::Context,
    streams::{input::InputStream, output::OutputStream},
    types::{ResourceAccess, ResourceType},
    ErrorCode, Message, WaferError,
};
use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::{
    clients::database::{self as db, LIST_ALL_MAX_ROWS},
    interfaces::database::{
        handler::handle_message,
        service::{pk, Column, DataType, DatabaseService, Table},
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

const TABLE: &str = "items";

/// A context over a table holding `rows` rows, ids `r00000`, `r00001`, ….
async fn ctx_with_rows(rows: u32) -> DbCtx {
    let svc = SQLiteDatabaseService::open_in_memory().expect("open in-memory sqlite");
    svc.ensure_schema_table(&Table {
        name: TABLE.to_string(),
        columns: vec![pk("id"), Column::new("n", DataType::Int)],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    })
    .await
    .expect("create items");
    let batch: Vec<HashMap<String, serde_json::Value>> = (0..rows)
        .map(|n| {
            HashMap::from([
                ("id".to_string(), serde_json::json!(format!("r{n:05}"))),
                ("n".to_string(), serde_json::json!(n)),
            ])
        })
        .collect();
    let inserted = svc.create_many(TABLE, batch).await.expect("seed items");
    assert_eq!(inserted, i64::from(rows));
    DbCtx { svc: Arc::new(svc) }
}

fn assert_out_of_range(result: Result<Vec<db::Record>, WaferError>, what: &str) {
    let err = result.expect_err(&format!("{what}: one row past the cap must be refused"));
    assert_eq!(
        err.code,
        ErrorCode::OutOfRange,
        "{what}: expected OUT_OF_RANGE, got {:?}: {}",
        err.code,
        err.message
    );
}

#[tokio::test]
async fn list_all_returns_every_row_up_to_the_cap() {
    let ctx = ctx_with_rows(LIST_ALL_MAX_ROWS).await;
    let rows = db::list_all(&ctx, TABLE, Vec::new())
        .await
        .expect("list_all at the cap");
    assert_eq!(rows.len(), LIST_ALL_MAX_ROWS as usize);
}

#[tokio::test]
async fn list_all_refuses_one_row_past_the_cap() {
    let ctx = ctx_with_rows(LIST_ALL_MAX_ROWS + 1).await;
    assert_out_of_range(db::list_all(&ctx, TABLE, Vec::new()).await, "list_all");
}

#[tokio::test]
async fn list_sorted_refuses_one_row_past_the_cap() {
    let ctx = ctx_with_rows(LIST_ALL_MAX_ROWS + 1).await;
    let sort = vec![wafer_block::db::SortField {
        field: "n".to_string(),
        desc: true,
    }];
    assert_out_of_range(
        db::list_sorted(&ctx, TABLE, Vec::new(), sort).await,
        "list_sorted",
    );
}

#[tokio::test]
async fn list_sorted_returns_every_row_in_order_up_to_the_cap() {
    let ctx = ctx_with_rows(LIST_ALL_MAX_ROWS).await;
    let sort = vec![wafer_block::db::SortField {
        field: "n".to_string(),
        desc: true,
    }];
    let rows = db::list_sorted(&ctx, TABLE, Vec::new(), sort)
        .await
        .expect("list_sorted at the cap");
    assert_eq!(rows.len(), LIST_ALL_MAX_ROWS as usize);
    assert_eq!(rows[0].id, format!("r{:05}", LIST_ALL_MAX_ROWS - 1));
    assert_eq!(rows[rows.len() - 1].id, "r00000");
}
