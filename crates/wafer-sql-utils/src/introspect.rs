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
}
