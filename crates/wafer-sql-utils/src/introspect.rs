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

/// Build query to list a table's columns with their declared types.
///
/// Returns `(sql, params)`. The result shape is identical across dialects —
/// one row per table column, in declaration order, with a `name` column and a
/// `decl_type` column (the type as the schema declares it; decide JSON-ness
/// with [`is_json_decl_type`]) — unlike [`build_table_info`], whose result
/// columns differ per backend. A missing table yields zero rows (not an error)
/// in both dialects.
///
/// SQLite: `SELECT name, type AS decl_type FROM pragma_table_info(?1) ORDER BY
/// cid` (the table-valued pragma function, available since SQLite 3.16, which —
/// unlike `PRAGMA table_info(...)` — accepts a bound parameter).
/// Postgres: `SELECT column_name AS name, data_type AS decl_type FROM
/// information_schema.columns WHERE table_schema='public' AND table_name=$1
/// ORDER BY ordinal_position`.
///
/// The table name is parameter-bound in both dialects, so this builder is
/// infallible — no identifier validation needed.
pub fn build_list_columns(table: &str, backend: Backend) -> (String, Vec<serde_json::Value>) {
    let params = vec![serde_json::Value::String(table.to_string())];
    match backend {
        Backend::Sqlite => (
            "SELECT name, type AS decl_type FROM pragma_table_info(?1) ORDER BY cid".to_string(),
            params,
        ),
        Backend::Postgres => (
            "SELECT column_name AS name, data_type AS decl_type FROM information_schema.columns WHERE table_schema='public' AND table_name=$1 ORDER BY ordinal_position"
                .to_string(),
            params,
        ),
    }
}

/// Whether a column declared with `decl_type` (a [`build_list_columns`]
/// `decl_type` value) holds JSON.
///
/// SQLite has no JSON storage class: a JSON column is one declared `JSON` (what
/// the DDL builders emit for [`DataType::Json`](wafer_schema::DataType::Json)
/// and for a lazily added column first written with an object or array), and
/// its values are JSON text. Postgres reports its native `json` and `jsonb`
/// types. Any other declaration, `TEXT` included, holds plain values: a string
/// stored there is never read back as JSON, however it looks.
#[must_use]
pub fn is_json_decl_type(decl_type: &str) -> bool {
    let decl = decl_type.trim();
    decl.eq_ignore_ascii_case("json") || decl.eq_ignore_ascii_case("jsonb")
}

/// Build query to list the columns of a table's primary key.
///
/// Returns `(sql, params)`. One `name` column per key column, in key order
/// (the order of the `PRIMARY KEY (...)` list), in both dialects. A table
/// with no primary key, or no such table, yields zero rows.
///
/// SQLite: `SELECT name FROM pragma_table_info(?1) WHERE pk > 0 ORDER BY pk`
/// (`pk` is the column's 1-based position in the key, `0` off it).
/// Postgres: the key columns of the table's `indisprimary` index in
/// `pg_catalog.pg_index`, in `indkey` order, for the table
/// `to_regclass('public.<table>')` names — the `public` schema
/// [`build_list_columns`] reads. `to_regclass` is `NULL` for a missing
/// table, so that case is zero rows rather than an error. `indkey` also lists
/// a `PRIMARY KEY (...) INCLUDE (...)` index's non-key columns after its
/// `indnkeyatts` key columns; those are cut off, since they do not identify
/// a row.
///
/// Postgres reads the system catalog rather than
/// `information_schema.table_constraints`: the information schema shows a
/// constraint only to a role that owns the table or holds a privilege other
/// than `SELECT` on it, so a read-only role would see no key at all and
/// every list would silently lose its tiebreak. `pg_catalog` is readable by
/// every role.
///
/// Names come back as the catalog spells them (not lowercased), so a caller
/// can quote them straight back into SQL. The table name is parameter-bound,
/// so this builder is infallible.
pub fn build_list_primary_key(table: &str, backend: Backend) -> (String, Vec<serde_json::Value>) {
    let params = vec![serde_json::Value::String(table.to_string())];
    match backend {
        Backend::Sqlite => (
            "SELECT name FROM pragma_table_info(?1) WHERE pk > 0 ORDER BY pk".to_string(),
            params,
        ),
        Backend::Postgres => (
            "SELECT a.attname::text AS name FROM pg_catalog.pg_index i \
             CROSS JOIN LATERAL unnest(i.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord) \
             JOIN pg_catalog.pg_attribute a \
             ON a.attrelid = i.indrelid AND a.attnum = k.attnum \
             WHERE i.indisprimary AND k.ord <= i.indnkeyatts \
             AND i.indrelid = to_regclass(format('public.%I', $1::text)) \
             ORDER BY k.ord"
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
        assert!(sql.contains("AS decl_type"), "{sql}");
        assert!(sql.contains("ORDER BY cid"), "{sql}");
        assert_eq!(params, vec![serde_json::json!("users")]);
    }

    #[test]
    fn only_json_and_jsonb_declare_a_json_column() {
        for decl in ["JSON", "json", " Json ", "jsonb", "JSONB"] {
            assert!(is_json_decl_type(decl), "{decl:?} declares JSON");
        }
        for decl in [
            "TEXT",
            "text",
            "",
            "VARCHAR",
            "JSON TEXT",
            "character varying",
        ] {
            assert!(!is_json_decl_type(decl), "{decl:?} does not declare JSON");
        }
    }

    #[test]
    fn test_list_columns_postgres() {
        let (sql, params) = build_list_columns("users", Backend::Postgres);
        assert!(sql.contains("information_schema.columns"), "{sql}");
        assert!(sql.contains("AS name"), "{sql}");
        assert!(sql.contains("data_type AS decl_type"), "{sql}");
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
            "CREATE TABLE widgets (id TEXT PRIMARY KEY, name TEXT, created_at TEXT, meta JSON)",
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
        let cols: Vec<(String, String)> = stmt
            .query_map([params[0].as_str().unwrap()], |r| {
                Ok((r.get("name")?, r.get("decl_type")?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let names: Vec<&str> = cols.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["id", "name", "created_at", "meta"]);
        let json: Vec<&str> = cols
            .iter()
            .filter(|(_, t)| is_json_decl_type(t))
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(
            json,
            vec!["meta"],
            "only the JSON-declared column holds JSON"
        );

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

    #[test]
    fn test_list_primary_key_postgres() {
        let (sql, params) = build_list_primary_key("users", Backend::Postgres);
        assert!(sql.contains("i.indisprimary"), "{sql}");
        assert!(
            !sql.contains("information_schema"),
            "the information schema hides keys from read-only roles: {sql}"
        );
        assert!(sql.contains("$1"), "table name must be bound: {sql}");
        assert!(sql.contains("ORDER BY k.ord"), "{sql}");
        assert!(
            sql.contains("k.ord <= i.indnkeyatts"),
            "INCLUDE columns are not key columns: {sql}"
        );
        assert_eq!(params, vec![serde_json::json!("users")]);
    }

    // The key columns, in key order rather than declaration order, for a
    // single-column key, a composite key, an INTEGER PRIMARY KEY, a table with
    // no key, and a missing table — on a real SQLite engine.
    #[test]
    fn test_list_primary_key_executes_in_sqlite() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE single (token_hash TEXT PRIMARY KEY, id TEXT);
             CREATE TABLE composite (role_id TEXT, user_id TEXT, created_at TEXT,
                 PRIMARY KEY (user_id, role_id));
             CREATE TABLE rowid_key (n INTEGER PRIMARY KEY AUTOINCREMENT, v TEXT);
             CREATE TABLE keyless (a TEXT, b TEXT);",
        )
        .unwrap();
        let key = |table: &str| -> Vec<String> {
            let (sql, params) = build_list_primary_key(table, Backend::Sqlite);
            let mut stmt = conn.prepare(&sql).unwrap();
            stmt.query_map([params[0].as_str().unwrap()], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(key("single"), vec!["token_hash"]);
        assert_eq!(key("composite"), vec!["user_id", "role_id"]);
        assert_eq!(key("rowid_key"), vec!["n"]);
        assert!(key("keyless").is_empty());
        assert!(key("no_such_table").is_empty());
    }
}
