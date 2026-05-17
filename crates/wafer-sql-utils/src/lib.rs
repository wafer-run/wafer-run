#![warn(missing_docs)]
//! Dialect-agnostic SQL builders used by Wafer's database backends.
//!
//! Provides composable helpers for query / aggregate / upsert / DDL /
//! introspection statements that render to SQLite or Postgres via
//! [`sea_query`]. All builders return `(sql, params)` so callers can hand
//! them to whichever driver they use.

/// Aggregate SQL builders — `COUNT`, `SUM`, `AVG`, daily buckets, and a
/// flexible grouped-aggregate query.
pub mod aggregate;
/// Dependency-free base64 encoder used by builders that need to embed
/// binary payloads (e.g. vector embeddings) in SQL text.
pub mod base64;
/// DDL builders — `CREATE TABLE`, `CREATE INDEX`, `ALTER TABLE ADD
/// COLUMN`, `DROP TABLE`, with dialect-specific type mapping.
pub mod ddl;
/// Identifier helpers — a runtime [`sea_query::Iden`] implementation and
/// an alphanumeric-only sanitiser for table / column names that have to
/// be interpolated rather than parameter-bound.
pub mod ident;
/// Introspection queries — list user tables, fetch column info, count
/// rows. Each emits the dialect-specific catalog query for SQLite or
/// Postgres.
pub mod introspect;
/// Filter / sort / pagination plumbing plus CRUD-shape builders
/// (`SELECT`, `INSERT`, `UPDATE`, `DELETE`, atomic increment).
pub mod query;
/// Upsert builders — generic `INSERT ... ON CONFLICT DO UPDATE` and the
/// atomic fixed-window rate-limit upsert.
pub mod upsert;
/// Conversions between [`serde_json::Value`] and [`sea_query::Value`] so
/// JSON-typed call sites can produce sea-query parameter bindings.
pub mod value;
/// SQL builders for vector-store schemas backed by `sqlite-vec` and FTS5.
pub mod vector;

/// Re-export sea_query::Value so consumers can reference the param type
/// without adding sea-query as a direct dependency.
pub use sea_query::Value as SeaValue;

/// Database backend dialect for SQL rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// SQLite dialect — quoted identifiers, `?` parameter placeholders,
    /// `PRAGMA table_info` for introspection.
    Sqlite,
    /// PostgreSQL dialect — quoted identifiers, `$N` numbered parameter
    /// placeholders, `information_schema` for introspection.
    Postgres,
}

// Render a sea_query statement to dialect-specific SQL + parameter values.
// Implemented as a macro because sea_query's statement types don't share a
// `build` trait, but per-backend dispatch is identical.
macro_rules! render_stmt {
    ($name:ident, $stmt:ty) => {
        pub(crate) fn $name(query: $stmt, backend: Backend) -> (String, Vec<sea_query::Value>) {
            use sea_query::{PostgresQueryBuilder, SqliteQueryBuilder};
            let (sql, sea_query::Values(values)) = match backend {
                Backend::Sqlite => query.build(SqliteQueryBuilder),
                Backend::Postgres => query.build(PostgresQueryBuilder),
            };
            (sql, values)
        }
    };
}

render_stmt!(render_select, sea_query::SelectStatement);
render_stmt!(render_insert, sea_query::InsertStatement);
render_stmt!(render_update, sea_query::UpdateStatement);
render_stmt!(render_delete, sea_query::DeleteStatement);

#[cfg(test)]
mod render_stmt_tests {
    use sea_query::{Alias, Asterisk, Expr, Func, Query};

    use super::*;

    fn count_select() -> sea_query::SelectStatement {
        let mut q = Query::select();
        q.expr_as(Func::count(Expr::col(Asterisk)), Alias::new("cnt"))
            .from(Alias::new("t"));
        q
    }

    #[test]
    fn render_select_sqlite_dialect() {
        let (sql, _values) = render_select(count_select(), Backend::Sqlite);
        assert!(sql.starts_with("SELECT"), "{sql}");
        assert!(sql.contains("\"t\""), "{sql}");
    }

    #[test]
    fn render_select_postgres_dialect() {
        let (sql, _values) = render_select(count_select(), Backend::Postgres);
        assert!(sql.starts_with("SELECT"), "{sql}");
        assert!(sql.contains("\"t\""), "{sql}");
    }
}
