//! `forward_database_service!` — the decorator form.
//!
//! A `DatabaseService` decorator (a cache, an auditor, a read-only guard) that
//! writes out only the operations it cares about silently inherits the *trait
//! defaults* for the rest. Those defaults are not pass-throughs: `take_where`
//! lists then deletes row by row, `delete_where_count` counts then deletes,
//! `ensure_schema_tables` loops. A decorator that inherits them therefore
//! bypasses the wrapped backend's atomic single-statement paths — which is
//! exactly the defect this macro exists to make unrepresentable.
//!
//! The macro's decorator form requires the author to state a mode for *every*
//! `DatabaseService` operation, so "I forgot `take_where`" is not expressible:
//! an incomplete ledger does not compile (pinned by the `compile_fail`
//! doctest on the macro itself).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use wafer_block::db::{Filter, ListOptions};
use wafer_core::interfaces::database::service::{
    AggregateSpec, Column, DatabaseError, DatabaseService, Record, RecordList, Table, UpsertSpec,
};

// ---------------------------------------------------------------------------
// A `DatabaseService` that records which of its methods were called.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RecordingDb {
    calls: Mutex<Vec<String>>,
}

impl RecordingDb {
    fn note(&self, name: &str) {
        self.calls.lock().unwrap().push(name.to_string());
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

fn row(id: &str) -> Record {
    Record {
        id: id.to_string(),
        data: HashMap::new(),
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl DatabaseService for RecordingDb {
    async fn get(&self, _collection: &str, id: &str) -> Result<Record, DatabaseError> {
        self.note("get");
        Ok(row(id))
    }

    async fn list(
        &self,
        _collection: &str,
        _opts: &ListOptions,
    ) -> Result<RecordList, DatabaseError> {
        self.note("list");
        Ok(RecordList {
            records: vec![row("r1")],
            total_count: 1,
            page: 1,
            page_size: 10,
        })
    }

    async fn create(
        &self,
        _collection: &str,
        _data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        self.note("create");
        Ok(row("r1"))
    }

    async fn update(
        &self,
        _collection: &str,
        id: &str,
        _data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        self.note("update");
        Ok(row(id))
    }

    async fn delete(&self, _collection: &str, _id: &str) -> Result<(), DatabaseError> {
        self.note("delete");
        Ok(())
    }

    async fn count(&self, _collection: &str, _filters: &[Filter]) -> Result<i64, DatabaseError> {
        self.note("count");
        Ok(1)
    }

    async fn sum(
        &self,
        _collection: &str,
        _field: &str,
        _filters: &[Filter],
    ) -> Result<f64, DatabaseError> {
        self.note("sum");
        Ok(1.0)
    }

    async fn query_raw(
        &self,
        _query: &str,
        _args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        self.note("query_raw");
        Ok(Vec::new())
    }

    async fn exec_raw(
        &self,
        _query: &str,
        _args: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        self.note("exec_raw");
        Ok(0)
    }

    async fn delete_where(
        &self,
        _collection: &str,
        _filters: &[Filter],
    ) -> Result<(), DatabaseError> {
        self.note("delete_where");
        Ok(())
    }

    async fn delete_where_count(
        &self,
        _collection: &str,
        _filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        self.note("delete_where_count");
        Ok(3)
    }

    async fn take_where(
        &self,
        _collection: &str,
        _filters: &[Filter],
    ) -> Result<Vec<Record>, DatabaseError> {
        self.note("take_where");
        Ok(vec![row("r1")])
    }

    async fn update_where(
        &self,
        _collection: &str,
        _filters: &[Filter],
        _data: HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        self.note("update_where");
        Ok(())
    }

    async fn update_where_count(
        &self,
        _collection: &str,
        _filters: &[Filter],
        _data: HashMap<String, serde_json::Value>,
    ) -> Result<i64, DatabaseError> {
        self.note("update_where_count");
        Ok(4)
    }

    async fn increment_field_where(
        &self,
        _collection: &str,
        _col: &str,
        _delta: i64,
        _filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        self.note("increment_field_where");
        Ok(1)
    }

    async fn upsert(&self, _collection: &str, _spec: UpsertSpec) -> Result<i64, DatabaseError> {
        self.note("upsert");
        Ok(1)
    }

    async fn aggregate(
        &self,
        _collection: &str,
        _spec: AggregateSpec,
    ) -> Result<Vec<Record>, DatabaseError> {
        self.note("aggregate");
        Ok(Vec::new())
    }

    async fn ensure_schema_table(&self, _table: &Table) -> Result<(), DatabaseError> {
        self.note("ensure_schema_table");
        Ok(())
    }

    async fn ensure_schema_tables(&self, _tables: &[Table]) -> Result<(), DatabaseError> {
        self.note("ensure_schema_tables");
        Ok(())
    }

    async fn schema_table_exists(&self, _name: &str) -> Result<bool, DatabaseError> {
        self.note("schema_table_exists");
        Ok(true)
    }

    async fn schema_drop_table(&self, _name: &str) -> Result<(), DatabaseError> {
        self.note("schema_drop_table");
        Ok(())
    }

    async fn schema_add_column(&self, _table: &str, _column: &Column) -> Result<(), DatabaseError> {
        self.note("schema_add_column");
        Ok(())
    }

    fn set_strict_schema(&self, _enabled: bool) {
        self.note("set_strict_schema");
    }
}

// ---------------------------------------------------------------------------
// A decorator built with the macro: everything forwards except `count`.
// ---------------------------------------------------------------------------

struct Decorator {
    inner: Arc<RecordingDb>,
}

impl Decorator {
    /// The wrapped service the `forward` ledger entries delegate to.
    fn inner_service(&self) -> &dyn DatabaseService {
        self.inner.as_ref()
    }
}

wafer_core::forward_database_service! {
    impl DatabaseService for Decorator {
        forward_to inner_service();

        ops {
            get: forward,
            list: forward,
            create: forward,
            update: forward,
            delete: forward,
            count: custom,
            sum: forward,
            query_raw: forward,
            exec_raw: forward,
            delete_where: forward,
            delete_where_count: forward,
            take_where: forward,
            update_where: forward,
            update_where_count: forward,
            increment_field_where: forward,
            upsert: forward,
            aggregate: forward,
            ensure_schema_table: forward,
            ensure_schema_tables: forward,
            schema_table_exists: forward,
            schema_drop_table: forward,
            schema_add_column: forward,
            set_strict_schema: forward,
        }

        /// Answered here rather than forwarded, to prove `custom` suppresses
        /// the generated method instead of colliding with it.
        async fn count(&self, _collection: &str, _filters: &[Filter]) -> Result<i64, DatabaseError> {
            Ok(-1)
        }
    }
}

fn decorated() -> (Decorator, Arc<RecordingDb>) {
    let inner = Arc::new(RecordingDb::default());
    (
        Decorator {
            inner: Arc::clone(&inner),
        },
        inner,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The bulk op whose trait default lists-then-deletes row by row. A forwarded
/// decorator must reach the backend's atomic `DELETE … RETURNING *`.
#[tokio::test]
async fn take_where_reaches_the_inner_service_instead_of_the_list_then_delete_default() {
    let (dec, inner) = decorated();
    let taken = dec.take_where("t", &[]).await.expect("take_where");
    assert_eq!(taken.len(), 1);
    assert_eq!(inner.calls(), vec!["take_where".to_string()]);
}

/// The bulk op whose trait default counts-then-deletes (a TOCTOU window).
#[tokio::test]
async fn delete_where_count_reaches_the_inner_service_instead_of_count_then_delete() {
    let (dec, inner) = decorated();
    assert_eq!(dec.delete_where_count("t", &[]).await.expect("dwc"), 3);
    assert_eq!(inner.calls(), vec!["delete_where_count".to_string()]);
}

/// `update_where` / `update_where_count` have the same defaulted shape.
#[tokio::test]
async fn update_where_family_reaches_the_inner_service() {
    let (dec, inner) = decorated();
    dec.update_where("t", &[], HashMap::new())
        .await
        .expect("update_where");
    assert_eq!(
        dec.update_where_count("t", &[], HashMap::new())
            .await
            .expect("uwc"),
        4
    );
    assert_eq!(
        inner.calls(),
        vec!["update_where".to_string(), "update_where_count".to_string()]
    );
}

/// `delete_where`'s default loops `list` + `delete` until the table drains.
#[tokio::test]
async fn delete_where_reaches_the_inner_service() {
    let (dec, inner) = decorated();
    dec.delete_where("t", &[]).await.expect("delete_where");
    assert_eq!(inner.calls(), vec!["delete_where".to_string()]);
}

/// `ensure_schema_tables`' default loops over `ensure_schema_table`, which a
/// decorator with per-table policy would then apply N times.
#[tokio::test]
async fn ensure_schema_tables_reaches_the_inner_service() {
    let (dec, inner) = decorated();
    dec.ensure_schema_tables(&[]).await.expect("est");
    assert_eq!(inner.calls(), vec!["ensure_schema_tables".to_string()]);
}

/// `increment_field_where`'s default is a hard error, so a decorator that
/// inherits it turns an atomic counter bump into a 500.
#[tokio::test]
async fn increment_field_where_reaches_the_inner_service() {
    let (dec, inner) = decorated();
    assert_eq!(
        dec.increment_field_where("t", "n", 1, &[]).await.unwrap(),
        1
    );
    assert_eq!(inner.calls(), vec!["increment_field_where".to_string()]);
}

/// `set_strict_schema` is the sync method; its default is a silent no-op, so a
/// decorator that inherits it leaves the wrapped backend in non-strict mode.
#[tokio::test]
async fn set_strict_schema_reaches_the_inner_service() {
    let (dec, inner) = decorated();
    dec.set_strict_schema(true);
    assert_eq!(inner.calls(), vec!["set_strict_schema".to_string()]);
}

/// Every remaining op forwards too.
#[tokio::test]
async fn the_rest_of_the_surface_forwards() {
    let (dec, inner) = decorated();
    dec.get("t", "r1").await.expect("get");
    dec.list("t", &ListOptions::default()).await.expect("list");
    dec.create("t", HashMap::new()).await.expect("create");
    dec.update("t", "r1", HashMap::new()).await.expect("update");
    dec.delete("t", "r1").await.expect("delete");
    dec.sum("t", "n", &[]).await.expect("sum");
    dec.query_raw("SELECT 1", &[]).await.expect("query_raw");
    dec.exec_raw("SELECT 1", &[]).await.expect("exec_raw");
    dec.schema_table_exists("t").await.expect("ste");
    dec.schema_drop_table("t").await.expect("sdt");
    assert_eq!(
        inner.calls(),
        vec![
            "get".to_string(),
            "list".to_string(),
            "create".to_string(),
            "update".to_string(),
            "delete".to_string(),
            "sum".to_string(),
            "query_raw".to_string(),
            "exec_raw".to_string(),
            "schema_table_exists".to_string(),
            "schema_drop_table".to_string(),
        ]
    );
}

/// A `custom` entry suppresses the generated forward so the hand-written body
/// in the same invocation is the one that runs.
#[tokio::test]
async fn a_custom_entry_is_answered_locally_and_never_reaches_the_inner_service() {
    let (dec, inner) = decorated();
    assert_eq!(dec.count("t", &[]).await.expect("count"), -1);
    assert!(inner.calls().is_empty());
}
