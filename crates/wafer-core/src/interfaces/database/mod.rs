/// Shared SQL-backend execution layer (`DbExec`) behind `DatabaseService`.
pub mod exec;
pub mod handler;
/// `DatabaseService` trait plus the schema, filter, and column builder types.
pub mod service;
