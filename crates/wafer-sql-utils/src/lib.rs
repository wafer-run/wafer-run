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
/// DDL builders — `CREATE TABLE`, `CREATE INDEX`, `ALTER TABLE ADD
/// COLUMN`, `DROP TABLE`, with dialect-specific type mapping.
pub mod ddl;
/// Identifier helpers — a runtime [`sea_query::Iden`] implementation and
/// a fail-closed validator for table / column names that have to be
/// interpolated rather than parameter-bound.
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

/// A typed SQL statement produced by a `build_*` helper, ready to hand to the
/// database service.
///
/// Carries the rendered SQL, its sea-query parameter values, and the primary
/// collection (table) the statement targets. The collection is used as the
/// WRAP resource when the statement reaches the database service handler, so
/// blocks declare their access at builder-construction time and the runtime
/// enforces it on dispatch.
///
/// Multi-table queries (joins, catalog introspection) don't fit this shape;
/// those callers continue to use the admin-only `exec_raw` / `query_raw`
/// client methods.
#[derive(Debug, Clone)]
pub struct Statement {
    /// Rendered SQL string in the appropriate dialect.
    pub sql: String,
    /// Positional parameter values, in the order the SQL references them.
    pub values: Vec<SeaValue>,
    /// Primary table this statement targets. Used as the WRAP resource.
    pub collection: String,
}

impl Statement {
    /// Construct a statement from a rendered SQL string, its parameter values,
    /// and the collection (table) it targets.
    pub fn new(sql: String, values: Vec<SeaValue>, collection: impl Into<String>) -> Self {
        Self {
            sql,
            values,
            collection: collection.into(),
        }
    }
}

/// Error returned by fallible SQL builders that validate caller-supplied
/// fragments before splicing them into DDL/DML text.
///
/// Builders that only ever interpolate parameter-bound values or sanitised
/// identifiers are infallible and return [`Statement`] directly; this type is
/// for the cases where a builder must reject input it cannot safely render
/// (e.g. an out-of-allowlist foreign-key action, or a date column that isn't a
/// plain identifier) rather than silently producing wrong or unsafe SQL.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SqlBuildError {
    /// A foreign-key referential action (`ON DELETE` / `ON UPDATE`) was
    /// outside the allowed set (CASCADE / SET NULL / SET DEFAULT / NO ACTION /
    /// RESTRICT). The action is not sanitised because doing so would silently
    /// corrupt multi-word actions like `SET NULL`.
    #[error("invalid foreign-key referential action: {action:?} (allowed: CASCADE, SET NULL, SET DEFAULT, NO ACTION, RESTRICT)")]
    InvalidFkAction {
        /// The rejected action string, as supplied by the caller.
        action: String,
    },
    /// An identifier that is interpolated into raw expression text (rather than
    /// quoted or parameter-bound) contained characters outside the plain
    /// identifier set (`[A-Za-z0-9_]`). Rejected so it can't break out of the
    /// surrounding SQL expression.
    #[error("identifier {value:?} is not a plain identifier (only ASCII alphanumerics and underscore are allowed)")]
    InvalidIdentifier {
        /// The rejected identifier, as supplied by the caller.
        value: String,
    },
}

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
mod statement_tests {
    use super::*;

    #[test]
    fn statement_carries_collection() {
        let s = Statement::new("SELECT 1".into(), vec![], "users");
        assert_eq!(s.sql, "SELECT 1");
        assert_eq!(s.collection, "users");
        assert!(s.values.is_empty());
    }

    #[test]
    fn statement_collection_accepts_string_and_str() {
        let from_str = Statement::new(String::new(), vec![], "t");
        let from_string = Statement::new(String::new(), vec![], String::from("t"));
        assert_eq!(from_str.collection, from_string.collection);
    }

    #[test]
    fn statement_is_clone_and_debug() {
        let s = Statement::new(
            "X".into(),
            vec![SeaValue::String(Some(Box::new("y".into())))],
            "t",
        );
        let s2 = s.clone();
        assert_eq!(s.sql, s2.sql);
        assert!(format!("{s:?}").contains("Statement"));
    }
}

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
