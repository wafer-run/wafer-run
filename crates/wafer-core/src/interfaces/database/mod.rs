/// Backend-agnostic [`DatabaseService`](service::DatabaseService) conformance
/// suite (`run_conformance`). Gated behind the `conformance` feature so it is
/// only compiled into the test builds of crates that opt in; never into a
/// production build.
#[cfg(feature = "conformance")]
pub mod conformance;
/// Shared SQL-backend execution layer (`DbExec`) behind `DatabaseService`.
pub mod exec;
pub use exec::{BatchOp, BatchResult};
pub mod handler;
/// Per-backend schema-introspection cache (`SchemaCache`) consulted by the
/// shared executor to elide redundant table-exists / column-list round-trips.
pub mod schema_cache;
/// Wire DTO (`wafer_block::wire::database::TableDef`/`ColumnDef`/...) →
/// `wafer_schema::{Table, Column}` conversion for the structured schema ops
/// (`ensure_table`, `add_column`). `pub` (not `pub(crate)`) because
/// `table_from_def` is exercised directly by
/// `tests/handler_database_schema_ops.rs` — see that function's doc comment.
pub mod schema_wire;
/// `DatabaseService` trait plus the schema, filter, and column builder types.
pub mod service;
