use crate::{ident::validate_ident, Backend, SqlBuildError};

/// Build query to list all user tables (excludes system tables).
///
/// SQLite: `SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name`
/// Postgres: `SELECT table_name AS name FROM information_schema.tables WHERE table_schema='public' ORDER BY table_name`
pub fn build_list_tables(backend: Backend) -> String {
    match backend {
        Backend::Sqlite => {
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name".to_string()
        }
        Backend::Postgres => {
            "SELECT table_name AS name FROM information_schema.tables WHERE table_schema='public' ORDER BY table_name".to_string()
        }
    }
}

/// Build query to list tables matching a name prefix.
///
/// Returns (sql, params) with one parameter for the LIKE pattern.
pub fn build_list_tables_like(prefix: &str, backend: Backend) -> (String, Vec<serde_json::Value>) {
    let pattern = format!("{prefix}%");
    match backend {
        Backend::Sqlite => (
            "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE ?1 ORDER BY name"
                .to_string(),
            vec![serde_json::Value::String(pattern)],
        ),
        Backend::Postgres => (
            "SELECT table_name AS name FROM information_schema.tables WHERE table_schema='public' AND table_name LIKE $1 ORDER BY table_name"
                .to_string(),
            vec![serde_json::Value::String(pattern)],
        ),
    }
}

/// Build query to check whether a table exists.
///
/// Returns `(sql, params)`. The result is a single row with one column
/// `present` — `1`/`0` on SQLite, `true`/`false` on Postgres — so callers can
/// decode it as a scalar in either dialect.
///
/// SQLite: `SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1) AS present`
/// Postgres: `SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_schema='public' AND table_name=$1) AS present`
///
/// The table name is parameter-bound in both dialects, so this builder is
/// infallible — no identifier validation needed.
pub fn build_table_exists(table: &str, backend: Backend) -> (String, Vec<serde_json::Value>) {
    let params = vec![serde_json::Value::String(table.to_string())];
    match backend {
        Backend::Sqlite => (
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1) AS present"
                .to_string(),
            params,
        ),
        Backend::Postgres => (
            "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_schema='public' AND table_name=$1) AS present"
                .to_string(),
            params,
        ),
    }
}

/// Build query to list a table's column names.
///
/// Returns `(sql, params)`. The result shape is identical across dialects —
/// one `name` column per table column, in declaration order — unlike
/// [`build_table_info`], whose result columns differ per backend. A missing
/// table yields zero rows (not an error) in both dialects.
///
/// SQLite: `SELECT name FROM pragma_table_info(?1) ORDER BY cid` (the
/// table-valued pragma function, available since SQLite 3.16, which —
/// unlike `PRAGMA table_info(...)` — accepts a bound parameter).
/// Postgres: `SELECT column_name AS name FROM information_schema.columns
/// WHERE table_schema='public' AND table_name=$1 ORDER BY ordinal_position`.
///
/// The table name is parameter-bound in both dialects, so this builder is
/// infallible — no identifier validation needed.
pub fn build_list_columns(table: &str, backend: Backend) -> (String, Vec<serde_json::Value>) {
    let params = vec![serde_json::Value::String(table.to_string())];
    match backend {
        Backend::Sqlite => (
            "SELECT name FROM pragma_table_info(?1) ORDER BY cid".to_string(),
            params,
        ),
        Backend::Postgres => (
            "SELECT column_name AS name FROM information_schema.columns WHERE table_schema='public' AND table_name=$1 ORDER BY ordinal_position"
                .to_string(),
            params,
        ),
    }
}

/// Build query to get column information for a table.
///
/// SQLite: `PRAGMA table_info("{table}")`
/// Postgres: `SELECT column_name, data_type, is_nullable, column_default FROM information_schema.columns WHERE table_name = $1`
///
/// Note: column names in the result differ between backends. SQLite returns `name`, `type`, `notnull`, `dflt_value`, `pk`.
/// Postgres returns `column_name`, `data_type`, `is_nullable`, `column_default`.
///
/// Returns [`SqlBuildError::InvalidIdentifier`] if `table` is not a plain
/// identifier (`[A-Za-z0-9_]`). The SQLite arm interpolates the name into the
/// `PRAGMA` text, so this is a fail-closed guard; the Postgres arm binds the
/// name but validates anyway so both dialects share one contract.
pub fn build_table_info(
    table: &str,
    backend: Backend,
) -> Result<(String, Vec<serde_json::Value>), SqlBuildError> {
    let safe = validate_ident(table)?;
    Ok(match backend {
        Backend::Sqlite => (format!("PRAGMA table_info(\"{safe}\")"), vec![]),
        Backend::Postgres => (
            "SELECT column_name, data_type, is_nullable, column_default FROM information_schema.columns WHERE table_name = $1 ORDER BY ordinal_position"
                .to_string(),
            vec![serde_json::Value::String(safe.to_string())],
        ),
    })
}

/// Build query to count rows in a table.
///
/// Returns [`SqlBuildError::InvalidIdentifier`] if `table` is not a plain
/// identifier (`[A-Za-z0-9_]`) — rejected rather than silently rewritten.
pub fn build_table_row_count(table: &str, _backend: Backend) -> Result<String, SqlBuildError> {
    let safe = validate_ident(table)?;
    Ok(format!("SELECT COUNT(*) AS cnt FROM \"{safe}\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_tables_sqlite() {
        let sql = build_list_tables(Backend::Sqlite);
        assert!(sql.contains("sqlite_master"));
    }

    #[test]
    fn test_list_tables_postgres() {
        let sql = build_list_tables(Backend::Postgres);
        assert!(sql.contains("information_schema"));
    }

    #[test]
    fn test_list_tables_like() {
        let (sql, params) = build_list_tables_like("custom_", Backend::Sqlite);
        assert!(sql.contains("LIKE"));
        assert_eq!(params[0], serde_json::json!("custom_%"));
    }

    #[test]
    fn test_table_info_sqlite() {
        let (sql, params) = build_table_info("users", Backend::Sqlite).expect("valid identifier");
        assert!(sql.contains("PRAGMA table_info"));
        assert!(params.is_empty());
    }

    #[test]
    fn test_table_info_postgres() {
        let (sql, params) = build_table_info("users", Backend::Postgres).expect("valid identifier");
        assert!(sql.contains("information_schema.columns"));
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_table_info_rejects_non_identifier() {
        // Fail-closed: the table name is interpolated into the PRAGMA text on
        // SQLite, so anything outside [A-Za-z0-9_] is rejected, not stripped.
        for backend in [Backend::Sqlite, Backend::Postgres] {
            let err = build_table_info("users\"); DROP TABLE x;--", backend)
                .expect_err("non-identifier table name must be rejected");
            assert!(matches!(err, SqlBuildError::InvalidIdentifier { .. }));
        }
    }

    #[test]
    fn test_table_row_count() {
        let sql = build_table_row_count("users", Backend::Sqlite).expect("valid identifier");
        assert_eq!(sql, "SELECT COUNT(*) AS cnt FROM \"users\"");
    }

    #[test]
    fn test_table_row_count_rejects_non_identifier() {
        let err = build_table_row_count("users; DROP TABLE", Backend::Sqlite)
            .expect_err("non-identifier table name must be rejected");
        assert!(matches!(err, SqlBuildError::InvalidIdentifier { .. }));
    }

    #[test]
    fn test_table_exists_sqlite() {
        let (sql, params) = build_table_exists("users", Backend::Sqlite);
        assert!(sql.contains("sqlite_master"), "{sql}");
        assert!(sql.contains("AS present"), "{sql}");
        assert!(sql.contains("?1"), "table name must be bound: {sql}");
        assert_eq!(params, vec![serde_json::json!("users")]);
    }

    #[test]
    fn test_table_exists_postgres() {
        let (sql, params) = build_table_exists("users", Backend::Postgres);
        assert!(sql.contains("information_schema.tables"), "{sql}");
        assert!(sql.contains("AS present"), "{sql}");
        assert!(sql.contains("$1"), "table name must be bound: {sql}");
        assert_eq!(params, vec![serde_json::json!("users")]);
    }

    #[test]
    fn test_list_columns_sqlite() {
        let (sql, params) = build_list_columns("users", Backend::Sqlite);
        assert!(sql.contains("pragma_table_info(?1)"), "{sql}");
        assert!(sql.contains("ORDER BY cid"), "{sql}");
        assert_eq!(params, vec![serde_json::json!("users")]);
    }

    #[test]
    fn test_list_columns_postgres() {
        let (sql, params) = build_list_columns("users", Backend::Postgres);
        assert!(sql.contains("information_schema.columns"), "{sql}");
        assert!(sql.contains("AS name"), "{sql}");
        assert!(sql.contains("ORDER BY ordinal_position"), "{sql}");
        assert_eq!(params, vec![serde_json::json!("users")]);
    }

    // Defense-in-depth: run the parameter-bound introspection SQL against a
    // real SQLite engine. `pragma_table_info(?1)` is the table-valued pragma
    // function form — prove the engine shipped with the workspace accepts a
    // bound table name, since that's the whole point of the builder.
    #[test]
    fn test_table_exists_and_list_columns_execute_in_sqlite() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE widgets (id TEXT PRIMARY KEY, name TEXT, created_at TEXT)",
            [],
        )
        .unwrap();

        let (sql, params) = build_table_exists("widgets", Backend::Sqlite);
        let present: i64 = conn
            .query_row(&sql, [params[0].as_str().unwrap()], |r| r.get(0))
            .unwrap();
        assert_eq!(present, 1);
        let (sql, params) = build_table_exists("no_such_table", Backend::Sqlite);
        let present: i64 = conn
            .query_row(&sql, [params[0].as_str().unwrap()], |r| r.get(0))
            .unwrap();
        assert_eq!(present, 0);

        let (sql, params) = build_list_columns("widgets", Backend::Sqlite);
        let mut stmt = conn.prepare(&sql).unwrap();
        let cols: Vec<String> = stmt
            .query_map([params[0].as_str().unwrap()], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(cols, vec!["id", "name", "created_at"]);

        // Missing table: zero rows, not an error.
        let (sql, params) = build_list_columns("no_such_table", Backend::Sqlite);
        let mut stmt = conn.prepare(&sql).unwrap();
        let cols: Vec<String> = stmt
            .query_map([params[0].as_str().unwrap()], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(cols.is_empty());
    }
}
