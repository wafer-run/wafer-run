use sea_query::{OnConflict, Query, SimpleExpr};

use crate::ident::DynCol;
use crate::value::json_to_sea_value;
use crate::Backend;

/// Build INSERT ... ON CONFLICT (conflict_cols) DO UPDATE SET {update_columns} = excluded.{col}.
///
/// This is the standard upsert pattern used for:
/// - Subscription upsert (stripe.rs)
/// - Block settings toggle (pages.rs)
pub fn build_upsert(
    table: &str,
    data: &[(String, serde_json::Value)],
    conflict_columns: &[&str],
    update_columns: &[&str],
    backend: Backend,
) -> (String, Vec<sea_query::Value>) {
    let mut query = Query::insert();
    query.into_table(DynCol(table.into()));

    let cols: Vec<DynCol> = data.iter().map(|(k, _)| DynCol(k.clone())).collect();
    let vals: Vec<SimpleExpr> = data
        .iter()
        .map(|(_, v)| json_to_sea_value(v).into())
        .collect();

    query.columns(cols);
    query.values_panic(vals);

    let mut on_conflict = OnConflict::columns(
        conflict_columns
            .iter()
            .map(|c| DynCol((*c).into()))
            .collect::<Vec<_>>(),
    );
    for col in update_columns {
        on_conflict.update_column(DynCol((*col).into()));
    }
    query.on_conflict(on_conflict);

    crate::render_insert(query, backend)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_upsert_sqlite() {
        let data = vec![
            ("id".to_string(), serde_json::json!("sub-1")),
            ("user_id".to_string(), serde_json::json!("user-1")),
            ("plan".to_string(), serde_json::json!("pro")),
            ("status".to_string(), serde_json::json!("active")),
        ];
        let (sql, values) = build_upsert(
            "subscriptions",
            &data,
            &["user_id"],
            &["plan", "status"],
            Backend::Sqlite,
        );
        assert!(sql.contains("INSERT INTO"));
        assert!(sql.contains("ON CONFLICT"));
        assert!(sql.contains("DO UPDATE SET"));
        assert_eq!(values.len(), 4);
    }

    #[test]
    fn test_build_upsert_postgres() {
        let data = vec![
            ("block_name".to_string(), serde_json::json!("auth")),
            ("enabled".to_string(), serde_json::json!(1)),
        ];
        let (sql, values) = build_upsert(
            "block_settings",
            &data,
            &["block_name"],
            &["enabled"],
            Backend::Postgres,
        );
        assert!(sql.contains("$1"));
        assert!(sql.contains("ON CONFLICT"));
        assert_eq!(values.len(), 2);
    }
}
