use wafer_schema::{Column, DataType, DefaultVal, DefaultValue, Index, Table};

use crate::{ident::sanitize_ident, Backend, SqlBuildError};

/// Quote an identifier for use in DDL (double-quote escaping).
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Validate a foreign-key referential action against an allowlist.
///
/// Returns the canonical uppercase form (`"CASCADE"`, `"SET NULL"`, etc.) so
/// it can be spliced directly into DDL. Unknown values are rejected — we do
/// NOT pass them through `sanitize_ident`, which strips spaces and would turn
/// `"SET NULL"` into `"SETNULL"` (silently breaking the constraint).
fn validate_fk_action(action: &str) -> Result<&'static str, SqlBuildError> {
    match action.trim().to_ascii_uppercase().as_str() {
        "CASCADE" => Ok("CASCADE"),
        "SET NULL" => Ok("SET NULL"),
        "SET DEFAULT" => Ok("SET DEFAULT"),
        "NO ACTION" => Ok("NO ACTION"),
        "RESTRICT" => Ok("RESTRICT"),
        _ => Err(SqlBuildError::InvalidFkAction {
            action: action.to_string(),
        }),
    }
}

fn data_type_to_sql(dt: DataType, backend: Backend) -> &'static str {
    match backend {
        Backend::Sqlite => match dt {
            DataType::String | DataType::Text => "TEXT",
            DataType::Int | DataType::Int64 => "INTEGER",
            DataType::Float => "REAL",
            DataType::Bool => "INTEGER",
            DataType::DateTime => "DATETIME",
            DataType::Json => "TEXT",
            DataType::Blob => "BLOB",
        },
        Backend::Postgres => match dt {
            DataType::String | DataType::Text => "TEXT",
            DataType::Int => "INTEGER",
            DataType::Int64 => "BIGINT",
            DataType::Float => "DOUBLE PRECISION",
            DataType::Bool => "BOOLEAN",
            DataType::DateTime => "TIMESTAMPTZ",
            DataType::Json => "JSONB",
            DataType::Blob => "BYTEA",
        },
    }
}

fn default_to_sql(d: &DefaultValue, backend: Backend) -> String {
    if d.is_null {
        return "NULL".to_string();
    }
    if d.is_raw {
        return match (backend, d.raw.as_str()) {
            (Backend::Postgres, "CURRENT_TIMESTAMP") => "NOW()".to_string(),
            _ => d.raw.clone(),
        };
    }
    match &d.value {
        Some(DefaultVal::String(s)) => format!("'{}'", s.replace('\'', "''")),
        Some(DefaultVal::Int(i)) => i.to_string(),
        Some(DefaultVal::Float(f)) => f.to_string(),
        Some(DefaultVal::Bool(b)) => match backend {
            Backend::Sqlite => if *b { "1" } else { "0" }.to_string(),
            Backend::Postgres => if *b { "TRUE" } else { "FALSE" }.to_string(),
        },
        None => "NULL".to_string(),
    }
}

fn column_to_sql(col: &Column, backend: Backend) -> String {
    let qname = quote_ident(&col.name);
    let mut sql = format!("{} {}", qname, data_type_to_sql(col.data_type, backend));

    if col.primary_key && !col.auto_increment {
        sql.push_str(" PRIMARY KEY");
    }

    if col.auto_increment {
        sql = match backend {
            Backend::Sqlite => format!("{qname} INTEGER PRIMARY KEY AUTOINCREMENT"),
            Backend::Postgres => {
                let s = format!("{qname} SERIAL PRIMARY KEY");
                if let Some(ref default) = col.default {
                    return format!("{} DEFAULT {}", s, default_to_sql(default, backend));
                }
                return s;
            }
        };
    }

    if !col.nullable && !col.primary_key {
        sql.push_str(" NOT NULL");
    }

    if col.unique && !col.primary_key {
        sql.push_str(" UNIQUE");
    }

    if let Some(ref default) = col.default {
        sql.push_str(" DEFAULT ");
        sql.push_str(&default_to_sql(default, backend));
    }

    sql
}

/// Generate a CREATE TABLE IF NOT EXISTS statement from a schema Table definition.
///
/// Returns `Err` if any column's foreign-key referential action is outside the
/// allowed set (CASCADE / SET NULL / SET DEFAULT / NO ACTION / RESTRICT).
pub fn build_create_table(
    table: &Table,
    backend: Backend,
) -> Result<crate::Statement, SqlBuildError> {
    let qtable = quote_ident(&table.name);
    let mut sql = format!("CREATE TABLE IF NOT EXISTS {qtable} (\n");

    for (i, col) in table.columns.iter().enumerate() {
        if i > 0 {
            sql.push_str(",\n");
        }
        sql.push_str("    ");
        sql.push_str(&column_to_sql(col, backend));
    }

    // Composite primary key
    if !table.primary_key.is_empty() {
        let quoted: Vec<String> = table.primary_key.iter().map(|k| quote_ident(k)).collect();
        sql.push_str(",\n    PRIMARY KEY(");
        sql.push_str(&quoted.join(", "));
        sql.push(')');
    }

    // Composite unique constraints
    for uk in &table.unique_keys {
        let quoted: Vec<String> = uk.iter().map(|k| quote_ident(k)).collect();
        sql.push_str(",\n    UNIQUE(");
        sql.push_str(&quoted.join(", "));
        sql.push(')');
    }

    // Foreign keys
    for col in &table.columns {
        if let Some(ref refs) = col.references {
            sql.push_str(",\n    FOREIGN KEY (");
            sql.push_str(&quote_ident(&col.name));
            sql.push_str(") REFERENCES ");
            sql.push_str(&quote_ident(&refs.table));
            sql.push('(');
            sql.push_str(&quote_ident(&refs.column));
            sql.push(')');
            if !refs.on_delete.is_empty() {
                sql.push_str(" ON DELETE ");
                sql.push_str(validate_fk_action(&refs.on_delete)?);
            }
            if !refs.on_update.is_empty() {
                sql.push_str(" ON UPDATE ");
                sql.push_str(validate_fk_action(&refs.on_update)?);
            }
        }
    }

    sql.push_str("\n)");
    Ok(crate::Statement::new(sql, vec![], table.name.clone()))
}

/// Generate a CREATE INDEX IF NOT EXISTS statement.
pub fn build_create_index(table_name: &str, idx: &Index, backend: Backend) -> crate::Statement {
    let _ = backend; // identical across backends
    let mut sql = String::from("CREATE ");
    if idx.unique {
        sql.push_str("UNIQUE ");
    }
    sql.push_str("INDEX IF NOT EXISTS ");

    let name = if idx.name.is_empty() {
        format!(
            "idx_{}_{}",
            sanitize_ident(table_name),
            idx.columns
                .iter()
                .map(|c| sanitize_ident(c))
                .collect::<Vec<_>>()
                .join("_")
        )
    } else {
        sanitize_ident(&idx.name)
    };
    sql.push_str(&name);
    sql.push_str(" ON ");
    sql.push_str(&quote_ident(table_name));
    sql.push('(');
    let quoted_cols: Vec<String> = idx.columns.iter().map(|c| quote_ident(c)).collect();
    sql.push_str(&quoted_cols.join(", "));
    sql.push(')');

    crate::Statement::new(sql, vec![], table_name)
}

/// Generate an ALTER TABLE ADD COLUMN statement.
pub fn build_add_column(table_name: &str, col: &Column, backend: Backend) -> crate::Statement {
    let sql = format!(
        "ALTER TABLE {} ADD COLUMN {}",
        quote_ident(table_name),
        column_to_sql(col, backend)
    );
    crate::Statement::new(sql, vec![], table_name)
}

/// Generate an `ALTER TABLE <table> ADD COLUMN <column> <type_sql>` statement.
///
/// Both `table_name` and `column_name` are quoted as identifiers. `type_sql`
/// is a dialect column type produced by `data_type_to_sql` (or, for the
/// lazy column-add path that maps from a `serde_json::Value`, the column type
/// the backend chose); it is spliced verbatim and must therefore be a trusted
/// type literal, never untrusted input.
///
/// This is the primitive the backends' lazy column-add paths use when they
/// know only a column name and a target type — not a full [`Column`] (which
/// [`build_add_column`] requires).
///
/// Postgres emits `ADD COLUMN IF NOT EXISTS` for idempotency: the lazy
/// column-add path may legitimately re-attempt the same add (two concurrent
/// inserts of a record carrying a new column, or when the in-memory existence
/// check and the stored column name differ), and Postgres surfaces a redundant
/// `ADD COLUMN` as a hard error. SQLite does **not** support `IF NOT EXISTS` on
/// `ADD COLUMN`, so its callers guard with an existence check and swallow the
/// duplicate-column error instead.
pub fn build_add_column_with_type(
    table_name: &str,
    column_name: &str,
    type_sql: &str,
    backend: Backend,
) -> crate::Statement {
    let if_not_exists = match backend {
        Backend::Postgres => "IF NOT EXISTS ",
        Backend::Sqlite => "",
    };
    let sql = format!(
        "ALTER TABLE {} ADD COLUMN {}{} {}",
        quote_ident(table_name),
        if_not_exists,
        quote_ident(column_name),
        type_sql
    );
    crate::Statement::new(sql, vec![], table_name)
}

/// Generate an `ALTER TABLE <table> ADD COLUMN <column> TEXT` statement.
///
/// Convenience wrapper over [`build_add_column_with_type`] for the lazy
/// column-add path, where filter/sort/data columns absent from the table are
/// synthesised as a TEXT column (defaulting to NULL). The TEXT type name is
/// identical across SQLite and Postgres, so it is shared via
/// `data_type_to_sql`.
pub fn build_add_text_column(
    table_name: &str,
    column_name: &str,
    backend: Backend,
) -> crate::Statement {
    let type_sql = data_type_to_sql(DataType::Text, backend);
    build_add_column_with_type(table_name, column_name, type_sql, backend)
}

/// Generate a DROP TABLE IF EXISTS statement.
pub fn build_drop_table(table_name: &str, _backend: Backend) -> crate::Statement {
    let sql = format!("DROP TABLE IF EXISTS {}", quote_ident(table_name));
    crate::Statement::new(sql, vec![], table_name)
}

#[cfg(test)]
mod tests {
    use wafer_schema::{col_string, pk, timestamps};

    use super::*;

    fn test_table() -> Table {
        Table {
            name: "users".into(),
            columns: {
                let mut cols = vec![pk("id"), col_string("name").not_null()];
                cols.extend(timestamps());
                cols
            },
            indexes: vec![],
            primary_key: vec![],
            unique_keys: vec![],
        }
    }

    #[test]
    fn test_create_table_sqlite() {
        let stmt = build_create_table(&test_table(), Backend::Sqlite).expect("valid");
        let sql = stmt.sql;
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS"));
        assert!(sql.contains("\"id\" TEXT PRIMARY KEY"));
        assert!(sql.contains("\"name\" TEXT NOT NULL"));
        assert!(sql.contains("\"created_at\" DATETIME"));
        assert!(sql.contains("CURRENT_TIMESTAMP"));
        assert_eq!(stmt.collection, "users");
    }

    #[test]
    fn test_create_table_postgres() {
        let stmt = build_create_table(&test_table(), Backend::Postgres).expect("valid");
        let sql = stmt.sql;
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS"));
        assert!(sql.contains("\"id\" TEXT PRIMARY KEY"));
        assert!(sql.contains("\"name\" TEXT NOT NULL"));
        assert!(sql.contains("\"created_at\" TIMESTAMPTZ"));
        assert!(sql.contains("NOW()"));
        assert_eq!(stmt.collection, "users");
    }

    fn fk_table(on_delete: &str, on_update: &str) -> Table {
        use wafer_schema::Reference;

        let mut author_col = col_string("author_id");
        author_col.references = Some(Reference {
            table: "users".into(),
            column: "id".into(),
            on_delete: on_delete.into(),
            on_update: on_update.into(),
        });

        Table {
            name: "posts".into(),
            columns: vec![pk("id"), author_col],
            indexes: vec![],
            primary_key: vec![],
            unique_keys: vec![],
        }
    }

    #[test]
    fn test_fk_action_set_null_survives() {
        let stmt =
            build_create_table(&fk_table("SET NULL", "CASCADE"), Backend::Sqlite).expect("valid");
        let sql = stmt.sql;
        assert!(
            sql.contains("ON DELETE SET NULL"),
            "expected `ON DELETE SET NULL` in: {sql}"
        );
        assert!(
            sql.contains("ON UPDATE CASCADE"),
            "expected `ON UPDATE CASCADE` in: {sql}"
        );
        assert_eq!(stmt.collection, "posts");
    }

    #[test]
    fn test_fk_action_case_insensitive() {
        let stmt =
            build_create_table(&fk_table("set null", "no action"), Backend::Sqlite).expect("valid");
        let sql = stmt.sql;
        // Output should be canonical uppercase regardless of input case.
        assert!(sql.contains("ON DELETE SET NULL"));
        assert!(sql.contains("ON UPDATE NO ACTION"));
    }

    #[test]
    fn test_fk_action_rejects_unknown() {
        let err = build_create_table(&fk_table("DROP TABLE", ""), Backend::Sqlite)
            .expect_err("unknown FK action should be rejected");
        assert_eq!(
            err,
            SqlBuildError::InvalidFkAction {
                action: "DROP TABLE".to_string()
            },
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_drop_table() {
        let stmt = build_drop_table("users", Backend::Sqlite);
        assert_eq!(stmt.sql, "DROP TABLE IF EXISTS \"users\"");
        assert_eq!(stmt.collection, "users");
    }

    #[test]
    fn test_add_text_column_sqlite() {
        let stmt = build_add_text_column("users", "nickname", Backend::Sqlite);
        // SQLite does not support `IF NOT EXISTS` on `ADD COLUMN`.
        assert_eq!(
            stmt.sql,
            "ALTER TABLE \"users\" ADD COLUMN \"nickname\" TEXT"
        );
        assert_eq!(stmt.collection, "users");
        assert!(stmt.values.is_empty());
    }

    #[test]
    fn test_add_text_column_postgres() {
        let stmt = build_add_text_column("users", "nickname", Backend::Postgres);
        // TEXT is identical across dialects; identifiers are quoted. Postgres
        // gets `IF NOT EXISTS` so a re-attempted lazy add is a no-op rather than
        // a hard error.
        assert_eq!(
            stmt.sql,
            "ALTER TABLE \"users\" ADD COLUMN IF NOT EXISTS \"nickname\" TEXT"
        );
        assert_eq!(stmt.collection, "users");
    }

    #[test]
    fn test_add_text_column_quotes_identifiers() {
        // A column name containing a double quote must be escaped, not
        // splatted into the DDL where it could break out of the identifier.
        let stmt = build_add_text_column("posts", "weird\"name", Backend::Sqlite);
        assert_eq!(
            stmt.sql,
            "ALTER TABLE \"posts\" ADD COLUMN \"weird\"\"name\" TEXT"
        );
    }

    #[test]
    fn test_add_column_with_type_postgres_typed() {
        let stmt = build_add_column_with_type("orders", "amount", "BIGINT", Backend::Postgres);
        assert_eq!(
            stmt.sql,
            "ALTER TABLE \"orders\" ADD COLUMN IF NOT EXISTS \"amount\" BIGINT"
        );
        assert_eq!(stmt.collection, "orders");
    }

    #[test]
    fn test_create_index() {
        let idx = Index {
            name: "".into(),
            columns: vec!["email".into()],
            unique: true,
        };
        let stmt = build_create_index("users", &idx, Backend::Sqlite);
        let sql = stmt.sql;
        assert!(sql.contains("CREATE UNIQUE INDEX IF NOT EXISTS"));
        assert!(sql.contains("idx_users_email"));
        assert!(sql.contains("ON \"users\""));
        assert_eq!(stmt.collection, "users");
    }
}
