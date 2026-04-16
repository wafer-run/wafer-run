use wafer_core::interfaces::database::service::{
    Column, DataType, DefaultVal, DefaultValue, Index, Table,
};

use crate::{ident::sanitize_ident, Backend};

/// Quote an identifier for use in DDL (double-quote escaping).
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
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
pub fn build_create_table(table: &Table, backend: Backend) -> String {
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
                sql.push_str(&sanitize_ident(&refs.on_delete));
            }
            if !refs.on_update.is_empty() {
                sql.push_str(" ON UPDATE ");
                sql.push_str(&sanitize_ident(&refs.on_update));
            }
        }
    }

    sql.push_str("\n)");
    sql
}

/// Generate a CREATE INDEX IF NOT EXISTS statement.
pub fn build_create_index(table_name: &str, idx: &Index, backend: Backend) -> String {
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

    sql
}

/// Generate an ALTER TABLE ADD COLUMN statement.
pub fn build_add_column(table_name: &str, col: &Column, backend: Backend) -> String {
    format!(
        "ALTER TABLE {} ADD COLUMN {}",
        quote_ident(table_name),
        column_to_sql(col, backend)
    )
}

/// Generate a DROP TABLE IF EXISTS statement.
pub fn build_drop_table(table_name: &str, _backend: Backend) -> String {
    format!("DROP TABLE IF EXISTS {}", quote_ident(table_name))
}

#[cfg(test)]
mod tests {
    use wafer_core::interfaces::database::service::{col_string, pk, timestamps};

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
        let sql = build_create_table(&test_table(), Backend::Sqlite);
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS"));
        assert!(sql.contains("\"id\" TEXT PRIMARY KEY"));
        assert!(sql.contains("\"name\" TEXT NOT NULL"));
        assert!(sql.contains("\"created_at\" DATETIME"));
        assert!(sql.contains("CURRENT_TIMESTAMP"));
    }

    #[test]
    fn test_create_table_postgres() {
        let sql = build_create_table(&test_table(), Backend::Postgres);
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS"));
        assert!(sql.contains("\"id\" TEXT PRIMARY KEY"));
        assert!(sql.contains("\"name\" TEXT NOT NULL"));
        assert!(sql.contains("\"created_at\" TIMESTAMPTZ"));
        assert!(sql.contains("NOW()"));
    }

    #[test]
    fn test_drop_table() {
        let sql = build_drop_table("users", Backend::Sqlite);
        assert_eq!(sql, "DROP TABLE IF EXISTS \"users\"");
    }

    #[test]
    fn test_create_index() {
        let idx = Index {
            name: "".into(),
            columns: vec!["email".into()],
            unique: true,
        };
        let sql = build_create_index("users", &idx, Backend::Sqlite);
        assert!(sql.contains("CREATE UNIQUE INDEX IF NOT EXISTS"));
        assert!(sql.contains("idx_users_email"));
        assert!(sql.contains("ON \"users\""));
    }
}
