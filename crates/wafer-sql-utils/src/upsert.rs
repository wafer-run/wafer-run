use sea_query::{OnConflict, Query, SimpleExpr};

use crate::{ident::DynCol, value::json_to_sea_value, Backend};

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

    let mut on_conflict = OnConflict::columns(conflict_columns.iter().map(|c| DynCol((*c).into())));
    for col in update_columns {
        on_conflict.update_column(DynCol((*col).into()));
    }
    query.on_conflict(on_conflict);

    crate::render_insert(query, backend)
}

/// Build the atomic rate-limit upsert for a fixed-window counter.
///
/// Emits the dialect-portable equivalent of:
///
/// ```sql
/// INSERT INTO {table} (id, key, count, window_start, created_at, updated_at)
/// VALUES (?, ?, 1, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
/// ON CONFLICT(key) DO UPDATE SET
///   count        = CASE WHEN window_start < ? THEN 1 ELSE count + 1 END,
///   window_start = CASE WHEN window_start < ? THEN ? ELSE window_start END,
///   updated_at   = CURRENT_TIMESTAMP
/// ```
///
/// Semantics:
/// - First request in a window: row is inserted with `count=1`,
///   `window_start=now`.
/// - Subsequent request inside the same window
///   (`stored window_start >= window_cutoff`): increments `count` by 1.
/// - First request in a new window
///   (`stored window_start < window_cutoff`): resets `count=1` and rolls
///   `window_start` forward to `now`.
///
/// The `key` column must carry a UNIQUE constraint (the conflict target).
///
/// Arguments:
/// - `id` — fresh per-call row identifier; only used when no existing row
///   for `key` is present.
/// - `key` — rate-limit bucket key (e.g. `"user:<id>:login"`).
/// - `now` — current epoch-seconds; recorded as `window_start` on insert
///   or on window-reset.
/// - `window_cutoff` — `now - window_secs`; rows whose `window_start` is
///   strictly less than this are treated as expired.
pub fn build_rate_limit_upsert(
    table: &str,
    id: &str,
    key: &str,
    now: i64,
    window_cutoff: i64,
    backend: Backend,
) -> (String, Vec<sea_query::Value>) {
    use sea_query::CaseStatement;

    let mut query = Query::insert();
    query.into_table(DynCol(table.into()));

    let columns: Vec<DynCol> = [
        "id",
        "key",
        "count",
        "window_start",
        "created_at",
        "updated_at",
    ]
    .iter()
    .map(|c| DynCol((*c).into()))
    .collect();
    query.columns(columns);

    let now_expr: SimpleExpr = sea_query::Expr::current_timestamp().into();
    query.values_panic([
        json_to_sea_value(&serde_json::Value::String(id.to_string())).into(),
        json_to_sea_value(&serde_json::Value::String(key.to_string())).into(),
        json_to_sea_value(&serde_json::json!(1i64)).into(),
        json_to_sea_value(&serde_json::json!(now)).into(),
        now_expr.clone(),
        now_expr.clone(),
    ]);

    // count = CASE WHEN window_start < ? THEN 1 ELSE count + 1 END
    let count_case: SimpleExpr = CaseStatement::new()
        .case(
            sea_query::Expr::col(DynCol("window_start".into())).lt(window_cutoff),
            SimpleExpr::Value(1i64.into()),
        )
        .finally(sea_query::Expr::col(DynCol("count".into())).add(1))
        .into();

    // window_start = CASE WHEN window_start < ? THEN ? ELSE window_start END
    let window_start_case: SimpleExpr = CaseStatement::new()
        .case(
            sea_query::Expr::col(DynCol("window_start".into())).lt(window_cutoff),
            SimpleExpr::Value(now.into()),
        )
        .finally(sea_query::Expr::col(DynCol("window_start".into())))
        .into();

    let mut on_conflict = OnConflict::column(DynCol("key".into()));
    on_conflict.value(DynCol("count".into()), count_case);
    on_conflict.value(DynCol("window_start".into()), window_start_case);
    on_conflict.value(DynCol("updated_at".into()), now_expr);
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

    #[test]
    fn rate_limit_upsert_sqlite_shape() {
        let (sql, values) = build_rate_limit_upsert(
            "rate_limits",
            "rl-id-1",
            "user:42:login",
            1_700_000_000,
            1_699_999_940, // 60s window
            Backend::Sqlite,
        );

        // Insert side
        assert!(sql.contains("INSERT INTO"), "missing INSERT: {sql}");
        assert!(sql.contains("\"rate_limits\""));
        // The six columns in insertion order
        for col in [
            "\"id\"",
            "\"key\"",
            "\"count\"",
            "\"window_start\"",
            "\"created_at\"",
            "\"updated_at\"",
        ] {
            assert!(sql.contains(col), "missing column {col} in: {sql}");
        }

        // Conflict + CASE WHEN logic
        assert!(sql.contains("ON CONFLICT"));
        assert!(sql.contains("\"key\""));
        assert!(sql.contains("CASE WHEN"), "missing CASE WHEN in: {sql}");
        // Both case branches should be present
        assert_eq!(
            sql.matches("CASE WHEN").count(),
            2,
            "expected two CASE WHEN expressions, got: {sql}"
        );
        // CURRENT_TIMESTAMP used for created_at/updated_at — dialect-portable
        assert!(
            sql.contains("CURRENT_TIMESTAMP"),
            "missing CURRENT_TIMESTAMP: {sql}"
        );

        // Bound values: id, key, count(1), window_start, then the two
        // window_cutoff comparisons and the window_start reset value.
        // sea-query inlines NULL/bool/numeric where safe, but parameterizes
        // strings — so we just assert the count is reasonable.
        assert!(
            !values.is_empty(),
            "expected bound values, got empty: {sql}"
        );
    }

    #[test]
    fn rate_limit_upsert_postgres_uses_dollar_params() {
        let (sql, _values) = build_rate_limit_upsert(
            "rate_limits",
            "rl-id-2",
            "user:42:signup",
            1_700_000_000,
            1_699_999_700,
            Backend::Postgres,
        );

        assert!(sql.contains("$1"), "postgres should use $-params: {sql}");
        assert!(sql.contains("CASE WHEN"));
        assert!(sql.contains("CURRENT_TIMESTAMP"));
        // sanity: postgres quote style
        assert!(sql.contains("\"rate_limits\""));
    }
}
