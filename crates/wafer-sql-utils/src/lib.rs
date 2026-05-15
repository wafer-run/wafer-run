pub mod aggregate;
pub mod base64;
pub mod ddl;
pub mod ident;
pub mod introspect;
pub mod query;
pub mod upsert;
pub mod value;
pub mod vector;

/// Re-export sea_query::Value so consumers can reference the param type
/// without adding sea-query as a direct dependency.
pub use sea_query::Value as SeaValue;

/// Database backend dialect for SQL rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Sqlite,
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
