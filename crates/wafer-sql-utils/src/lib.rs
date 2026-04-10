pub mod aggregate;
pub mod base64;
pub mod ddl;
pub mod ident;
pub mod introspect;
pub mod query;
pub mod upsert;
pub mod value;

/// Database backend dialect for SQL rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Sqlite,
    Postgres,
}

pub(crate) fn render_select(
    query: sea_query::SelectStatement,
    backend: Backend,
) -> (String, Vec<sea_query::Value>) {
    use sea_query::{PostgresQueryBuilder, SqliteQueryBuilder};
    let (sql, sea_query::Values(values)) = match backend {
        Backend::Sqlite => query.build(SqliteQueryBuilder),
        Backend::Postgres => query.build(PostgresQueryBuilder),
    };
    (sql, values)
}

pub(crate) fn render_insert(
    query: sea_query::InsertStatement,
    backend: Backend,
) -> (String, Vec<sea_query::Value>) {
    use sea_query::{PostgresQueryBuilder, SqliteQueryBuilder};
    let (sql, sea_query::Values(values)) = match backend {
        Backend::Sqlite => query.build(SqliteQueryBuilder),
        Backend::Postgres => query.build(PostgresQueryBuilder),
    };
    (sql, values)
}

pub(crate) fn render_update(
    query: sea_query::UpdateStatement,
    backend: Backend,
) -> (String, Vec<sea_query::Value>) {
    use sea_query::{PostgresQueryBuilder, SqliteQueryBuilder};
    let (sql, sea_query::Values(values)) = match backend {
        Backend::Sqlite => query.build(SqliteQueryBuilder),
        Backend::Postgres => query.build(PostgresQueryBuilder),
    };
    (sql, values)
}

pub(crate) fn render_delete(
    query: sea_query::DeleteStatement,
    backend: Backend,
) -> (String, Vec<sea_query::Value>) {
    use sea_query::{PostgresQueryBuilder, SqliteQueryBuilder};
    let (sql, sea_query::Values(values)) = match backend {
        Backend::Sqlite => query.build(SqliteQueryBuilder),
        Backend::Postgres => query.build(PostgresQueryBuilder),
    };
    (sql, values)
}
