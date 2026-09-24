//! The `DatabaseService::delete_where` trait default, over a backend that
//! keeps its rows in memory: it deletes every match across several list
//! pages, and it fails, rather than looping forever, when a `delete` returns
//! `Ok` but the row is still there.

use std::{collections::HashMap, sync::Mutex, time::Duration};

use async_trait::async_trait;
use wafer_block::db::{Filter, ListOptions};
use wafer_core::interfaces::database::service::{
    AggregateSpec, CapGuard, DatabaseError, DatabaseService, GuardedInsert, GuardedUpdate, Record,
    RecordList, UpsertSpec, WriteOp, WriteOutcome,
};
use wafer_schema::{Column, Table};

/// Rows are ids only; every row matches every filter. `list` honours
/// `limit`. `delete` removes the row only when `deletes_take_effect`.
struct MemDb {
    rows: Mutex<Vec<String>>,
    deletes_take_effect: bool,
}

impl MemDb {
    fn with_rows(n: usize, deletes_take_effect: bool) -> Self {
        Self {
            rows: Mutex::new((0..n).map(|i| format!("r{i}")).collect()),
            deletes_take_effect,
        }
    }
}

fn unused(op: &str) -> DatabaseError {
    DatabaseError::Internal(format!("MemDb: {op} is not used by delete_where"))
}

#[async_trait]
impl DatabaseService for MemDb {
    async fn list(
        &self,
        _collection: &str,
        opts: &ListOptions,
    ) -> Result<RecordList, DatabaseError> {
        // Yield, so the test's timeout can fire if the caller never stops
        // listing.
        tokio::task::yield_now().await;
        let records: Vec<Record> = {
            let rows = self.rows.lock().unwrap();
            let take = opts.limit.map_or(rows.len(), |l| l as usize);
            rows.iter()
                .take(take)
                .map(|id| Record {
                    id: id.clone(),
                    data: HashMap::new(),
                })
                .collect()
        };
        Ok(RecordList {
            total_count: records.len() as i64,
            page: 1,
            page_size: records.len() as i64,
            records,
        })
    }
    async fn delete(&self, _collection: &str, id: &str) -> Result<(), DatabaseError> {
        if self.deletes_take_effect {
            self.rows.lock().unwrap().retain(|r| r != id);
        }
        Ok(())
    }

    async fn get(&self, _collection: &str, _id: &str) -> Result<Record, DatabaseError> {
        Err(unused("get"))
    }
    async fn create(
        &self,
        _collection: &str,
        _data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        Err(unused("create"))
    }
    async fn create_many(
        &self,
        _collection: &str,
        _rows: Vec<HashMap<String, serde_json::Value>>,
    ) -> Result<i64, DatabaseError> {
        Err(unused("create_many"))
    }
    async fn batch(&self, _ops: Vec<WriteOp>) -> Result<Vec<WriteOutcome>, DatabaseError> {
        Err(unused("batch"))
    }
    async fn insert_guarded(
        &self,
        _collection: &str,
        _data: HashMap<String, serde_json::Value>,
        _guards: &[CapGuard],
    ) -> Result<GuardedInsert, DatabaseError> {
        Err(unused("insert_guarded"))
    }
    async fn update_guarded(
        &self,
        _collection: &str,
        _filters: &[Filter],
        _data: HashMap<String, serde_json::Value>,
        _guards: &[CapGuard],
    ) -> Result<GuardedUpdate, DatabaseError> {
        Err(unused("update_guarded"))
    }
    async fn update(
        &self,
        _collection: &str,
        _id: &str,
        _data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        Err(unused("update"))
    }
    async fn count(&self, _collection: &str, _filters: &[Filter]) -> Result<i64, DatabaseError> {
        Err(unused("count"))
    }
    async fn sum(
        &self,
        _collection: &str,
        _field: &str,
        _filters: &[Filter],
    ) -> Result<f64, DatabaseError> {
        Err(unused("sum"))
    }
    async fn query_raw(
        &self,
        _query: &str,
        _args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        Err(unused("query_raw"))
    }
    async fn exec_raw(
        &self,
        _query: &str,
        _args: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        Err(unused("exec_raw"))
    }
    async fn take_where(
        &self,
        _collection: &str,
        _filters: &[Filter],
    ) -> Result<Vec<Record>, DatabaseError> {
        Err(unused("take_where"))
    }
    async fn update_where(
        &self,
        _collection: &str,
        _filters: &[Filter],
        _data: HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        Err(unused("update_where"))
    }
    async fn upsert(&self, _collection: &str, _spec: UpsertSpec) -> Result<i64, DatabaseError> {
        Err(unused("upsert"))
    }
    async fn aggregate(
        &self,
        _collection: &str,
        _spec: AggregateSpec,
    ) -> Result<Vec<Record>, DatabaseError> {
        Err(unused("aggregate"))
    }
    async fn ensure_schema_table(&self, _table: &Table) -> Result<(), DatabaseError> {
        Err(unused("ensure_schema_table"))
    }
    async fn schema_table_exists(&self, _name: &str) -> Result<bool, DatabaseError> {
        Err(unused("schema_table_exists"))
    }
    async fn schema_columns(&self, _table: &str) -> Result<Vec<String>, DatabaseError> {
        Err(unused("schema_columns"))
    }
    async fn schema_drop_table(&self, _name: &str) -> Result<(), DatabaseError> {
        Err(unused("schema_drop_table"))
    }
    async fn schema_add_column(&self, _table: &str, _column: &Column) -> Result<(), DatabaseError> {
        Err(unused("schema_add_column"))
    }
}

/// Upper bound on a call that must return: without the progress check the
/// default spins on the same rows, so the test fails here instead of hanging.
const RETURNS_WITHIN: Duration = Duration::from_secs(5);

#[tokio::test]
async fn delete_where_default_deletes_every_match_across_pages() {
    // More rows than one 10 000-row list page, so the loop takes two passes.
    let db = MemDb::with_rows(10_500, true);
    tokio::time::timeout(RETURNS_WITHIN, db.delete_where("t", &[]))
        .await
        .expect("delete_where returns")
        .expect("delete_where succeeds");
    assert!(db.rows.lock().unwrap().is_empty());
}

#[tokio::test]
async fn delete_where_default_fails_when_a_delete_does_not_take_effect() {
    let db = MemDb::with_rows(3, false);
    let err = tokio::time::timeout(RETURNS_WITHIN, db.delete_where("t", &[]))
        .await
        .expect("delete_where must return, not loop forever")
        .expect_err("rows that survive their delete must fail the call");
    match err {
        DatabaseError::Internal(msg) => assert!(
            msg.contains("still matches after its delete succeeded"),
            "{msg}"
        ),
        other => panic!("expected Internal, got {other:?}"),
    }
}
