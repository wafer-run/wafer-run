/// Shared SQL-backend execution layer (`DbExec`) behind `DatabaseService`.
pub mod exec;
pub mod handler;
/// Per-backend schema-introspection cache (`SchemaCache`) consulted by the
/// shared executor to elide redundant table-exists / column-list round-trips.
pub mod schema_cache;
/// `DatabaseService` trait plus the schema, filter, and column builder types.
pub mod service;
