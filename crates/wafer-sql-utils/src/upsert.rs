use sea_query::{OnConflict, Query, SimpleExpr};

use crate::{ident::DynCol, value::json_to_sea_value, Backend};

/// Build INSERT ... ON CONFLICT (conflict_cols) DO UPDATE SET {update_columns} = excluded.{col}.
///
/// When `update_columns` is empty, emits `ON CONFLICT (conflict_cols) DO NOTHING`
/// — the INSERT-OR-IGNORE semantic. Without this, sea-query produces an
/// incomplete `ON CONFLICT (...)` clause with no action, which sqlite rejects
/// at prepare time with "incomplete input" (see gizza-ai #68 outage).
///
/// This is the standard upsert pattern used for:
/// - Subscription upsert (stripe.rs)
/// - Block settings toggle (pages.rs)
/// - Bootstrap variable seeding (gizza-ai/config.rs) — uses the DO NOTHING form
pub fn build_upsert(
    table: &str,
    data: &[(String, serde_json::Value)],
    conflict_columns: &[&str],
    update_columns: &[&str],
    backend: Backend,
) -> crate::Statement {
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
    if update_columns.is_empty() {
        on_conflict.do_nothing();
    } else {
        for col in update_columns {
            on_conflict.update_column(DynCol((*col).into()));
        }
    }
    query.on_conflict(on_conflict);

    let (sql, values) = crate::render_insert(query, backend);
    crate::Statement::new(sql, values, table)
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
) -> crate::Statement {
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

    let (sql, values) = crate::render_insert(query, backend);
    crate::Statement::new(sql, values, table)
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
        let stmt = build_upsert(
            "subscriptions",
            &data,
            &["user_id"],
            &["plan", "status"],
            Backend::Sqlite,
        );
        let sql = stmt.sql;
        let values = stmt.values;
        assert!(sql.contains("INSERT INTO"));
        assert!(sql.contains("ON CONFLICT"));
        assert!(sql.contains("DO UPDATE SET"));
        assert_eq!(values.len(), 4);
        assert_eq!(stmt.collection, "subscriptions");
    }

    // Regression: empty update_columns must produce valid `DO NOTHING`, not
    // an incomplete `ON CONFLICT (key)` with no action clause. Reproduces the
    // gizza-ai #68 bootstrap outage where every cold start failed with sqlite
    // "incomplete input" because seed_and_load_variables relied on this
    // pattern for INSERT-OR-IGNORE semantics.
    #[test]
    fn test_build_upsert_empty_update_emits_do_nothing_sqlite() {
        let data = vec![
            ("id".to_string(), serde_json::json!("var-1")),
            ("key".to_string(), serde_json::json!("ADMIN_EMAIL")),
            ("value".to_string(), serde_json::json!("admin@example.com")),
        ];
        let stmt = build_upsert("variables", &data, &["key"], &[], Backend::Sqlite);
        assert!(
            stmt.sql.contains("DO NOTHING"),
            "empty update_columns must produce DO NOTHING, got: {}",
            stmt.sql
        );
    }

    #[test]
    fn test_build_upsert_empty_update_emits_do_nothing_postgres() {
        let data = vec![
            ("id".to_string(), serde_json::json!("var-1")),
            ("key".to_string(), serde_json::json!("ADMIN_EMAIL")),
            ("value".to_string(), serde_json::json!("admin@example.com")),
        ];
        let stmt = build_upsert("variables", &data, &["key"], &[], Backend::Postgres);
        assert!(
            stmt.sql.contains("DO NOTHING"),
            "empty update_columns must produce DO NOTHING, got: {}",
            stmt.sql
        );
    }

    // Defense-in-depth: prepare the generated SQL against a real sqlite engine
    // so the malformed-output class of bug surfaces in CI, not in production
    // after a deploy. String-level `contains("ON CONFLICT")` assertions
    // tolerate the broken `ON CONFLICT (key)` (no action) shape that
    // crashed gizza.ai.
    #[test]
    fn test_build_upsert_empty_update_parses_in_sqlite() {
        let data = vec![
            ("id".to_string(), serde_json::json!("var-1")),
            ("key".to_string(), serde_json::json!("ADMIN_EMAIL")),
            ("value".to_string(), serde_json::json!("admin@example.com")),
        ];
        let stmt = build_upsert("variables", &data, &["key"], &[], Backend::Sqlite);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE variables (id TEXT PRIMARY KEY, key TEXT UNIQUE, value TEXT)",
            [],
        )
        .unwrap();
        conn.prepare(&stmt.sql)
            .unwrap_or_else(|e| panic!("sqlite rejected upsert sql: {e}\nsql: {}", stmt.sql));
    }

    #[test]
    fn test_build_upsert_postgres() {
        let data = vec![
            ("block_name".to_string(), serde_json::json!("auth")),
            ("enabled".to_string(), serde_json::json!(1)),
        ];
        let stmt = build_upsert(
            "block_settings",
            &data,
            &["block_name"],
            &["enabled"],
            Backend::Postgres,
        );
        let sql = stmt.sql;
        let values = stmt.values;
        assert!(sql.contains("$1"));
        assert!(sql.contains("ON CONFLICT"));
        assert_eq!(values.len(), 2);
        assert_eq!(stmt.collection, "block_settings");
    }

    #[test]
    fn rate_limit_upsert_sqlite_shape() {
        let stmt = build_rate_limit_upsert(
            "rate_limits",
            "rl-id-1",
            "user:42:login",
            1_700_000_000,
            1_699_999_940, // 60s window
            Backend::Sqlite,
        );
        let sql = stmt.sql;
        let values = stmt.values;

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
        assert_eq!(stmt.collection, "rate_limits");
    }

    #[test]
    fn rate_limit_upsert_postgres_uses_dollar_params() {
        let stmt = build_rate_limit_upsert(
            "rate_limits",
            "rl-id-2",
            "user:42:signup",
            1_700_000_000,
            1_699_999_700,
            Backend::Postgres,
        );
        let sql = stmt.sql;
        assert!(sql.contains("$1"), "postgres should use $-params: {sql}");
        assert!(sql.contains("CASE WHEN"));
        assert!(sql.contains("CURRENT_TIMESTAMP"));
        // sanity: postgres quote style
        assert!(sql.contains("\"rate_limits\""));
        assert_eq!(stmt.collection, "rate_limits");
    }
}
