use sea_query::{Asterisk, Cond, Expr, Order, Query, SelectStatement, SimpleExpr};
use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};

use crate::{ident::DynCol, value::json_to_sea_value, Backend};

/// Convert a slice of Filters into a sea_query Cond.
/// Returns None if filters is empty.
pub fn build_condition(filters: &[Filter]) -> Option<Cond> {
    if filters.is_empty() {
        return None;
    }

    let mut cond = Cond::all();

    for filter in filters {
        let col = DynCol(filter.field.clone());
        let expr: SimpleExpr = match filter.operator {
            FilterOp::IsNull => Expr::col(col).is_null(),
            FilterOp::IsNotNull => Expr::col(col).is_not_null(),
            FilterOp::In => {
                if let serde_json::Value::Array(arr) = &filter.value {
                    let values: Vec<sea_query::Value> = arr.iter().map(json_to_sea_value).collect();
                    Expr::col(col).is_in(values)
                } else {
                    // Fail-safe: an `In` filter whose value isn't a JSON array
                    // is malformed input. Emit an always-false predicate rather
                    // than dropping the filter — narrowing the result set to
                    // nothing is safe; widening it (by skipping the predicate)
                    // would leak rows the caller meant to exclude.
                    Expr::cust("1=0")
                }
            }
            FilterOp::Equal => Expr::col(col).eq(json_to_sea_value(&filter.value)),
            FilterOp::NotEqual => Expr::col(col).ne(json_to_sea_value(&filter.value)),
            FilterOp::GreaterThan => Expr::col(col).gt(json_to_sea_value(&filter.value)),
            FilterOp::GreaterEqual => Expr::col(col).gte(json_to_sea_value(&filter.value)),
            FilterOp::LessThan => Expr::col(col).lt(json_to_sea_value(&filter.value)),
            FilterOp::LessEqual => Expr::col(col).lte(json_to_sea_value(&filter.value)),
            FilterOp::Like => {
                if let Some(pattern) = filter.value.as_str() {
                    Expr::col(col).like(pattern.to_string())
                } else {
                    // Fail-safe: a `Like` filter whose value isn't a JSON
                    // string is malformed input. Emit an always-false predicate
                    // rather than coercing to `LIKE ''` (which matches only
                    // empty strings — a surprising, non-failing result). Same
                    // narrow-never-widen rule as the `In` arm above.
                    Expr::cust("1=0")
                }
            }
        };
        cond = cond.add(expr);
    }

    Some(cond)
}

/// Apply sort directives to a SelectStatement.
pub fn apply_order(query: &mut SelectStatement, sort: &[SortField]) {
    for s in sort {
        let order = if s.desc { Order::Desc } else { Order::Asc };
        query.order_by(DynCol(s.field.clone()), order);
    }
}

/// Apply limit and offset to a SelectStatement.
pub fn apply_pagination(query: &mut SelectStatement, limit: i64, offset: i64) {
    if limit > 0 {
        query.limit(limit as u64);
    }
    if offset > 0 {
        query.offset(offset as u64);
    }
}

/// Build SELECT * FROM {table} with filters, sort, limit, offset.
pub fn build_select(table: &str, opts: &ListOptions, backend: Backend) -> crate::Statement {
    build_select_with_condition(table, opts, None, backend)
}

/// Build SELECT * FROM {table} with filters, sort, limit, offset, plus an
/// optional extra sea-query `Cond` AND-ed with the filters clause.
///
/// Use this when you need a complex WHERE — most commonly an OR group —
/// alongside the flat AND-of-filters list, without giving up `SELECT *`.
/// See [`build_select_columns`] for the projection variant and an OR-group
/// example.
pub fn build_select_with_condition(
    table: &str,
    opts: &ListOptions,
    extra_condition: Option<Cond>,
    backend: Backend,
) -> crate::Statement {
    let mut query = Query::select();
    query.column(Asterisk).from(DynCol(table.into()));

    if let Some(cond) = build_condition(&opts.filters) {
        query.cond_where(cond);
    }
    if let Some(extra) = extra_condition {
        query.cond_where(extra);
    }
    apply_order(&mut query, &opts.sort);
    apply_pagination(&mut query, opts.limit, opts.offset);

    let (sql, values) = crate::render_select(query, backend);
    crate::Statement::new(sql, values, table)
}

/// Build SELECT {columns} FROM {table} with filters, sort, limit, offset.
///
/// `extra_condition` is a sea-query `Cond` that is AND-ed with the
/// `opts.filters` clause. Use it for conditions that don't fit the flat
/// AND-of-filters model — for example, an OR group:
///
/// ```ignore
/// use sea_query::{Cond, Expr};
/// use wafer_sql_utils::{ident::DynCol, query, Backend};
/// use wafer_block::db::ListOptions;
///
/// let or_group = Cond::any()
///     .add(Expr::col(DynCol("email".into())).like("%alice%".to_string()))
///     .add(Expr::col(DynCol("id".into())).like("%alice%".to_string()));
///
/// let stmt = query::build_select_columns(
///     "users",
///     &["id", "email"],
///     &ListOptions::default(),
///     Some(or_group),
///     Backend::Sqlite,
/// );
/// ```
pub fn build_select_columns(
    table: &str,
    columns: &[&str],
    opts: &ListOptions,
    extra_condition: Option<Cond>,
    backend: Backend,
) -> crate::Statement {
    let mut query = Query::select();
    for col in columns {
        query.column(DynCol((*col).into()));
    }
    query.from(DynCol(table.into()));

    if let Some(cond) = build_condition(&opts.filters) {
        query.cond_where(cond);
    }
    if let Some(extra) = extra_condition {
        query.cond_where(extra);
    }
    apply_order(&mut query, &opts.sort);
    apply_pagination(&mut query, opts.limit, opts.offset);

    let (sql, values) = crate::render_select(query, backend);
    crate::Statement::new(sql, values, table)
}

/// Build INSERT INTO {table} (cols) VALUES (vals).
pub fn build_insert(
    table: &str,
    data: &[(String, serde_json::Value)],
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

    let (sql, values) = crate::render_insert(query, backend);
    crate::Statement::new(sql, values, table)
}

/// Build SELECT * FROM {table} WHERE id = {id}.
///
/// Caller-facing replacement for the `format!("SELECT * FROM {table}
/// WHERE id = ?1")` pattern. The placeholder syntax (`?1` for SQLite,
/// `$1` for Postgres) is selected from `backend`.
pub fn build_select_by_id(table: &str, id: &str, backend: Backend) -> crate::Statement {
    let mut query = Query::select();
    query
        .column(Asterisk)
        .from(DynCol(table.into()))
        .and_where(Expr::col(DynCol("id".into())).eq(id));
    let (sql, values) = crate::render_select(query, backend);
    crate::Statement::new(sql, values, table)
}

/// Build UPDATE {table} SET ... WHERE id = {id}.
pub fn build_update_by_id(
    table: &str,
    id: &str,
    data: &[(String, serde_json::Value)],
    backend: Backend,
) -> crate::Statement {
    let mut query = Query::update();
    query.table(DynCol(table.into()));

    for (col, val) in data {
        query.value(DynCol(col.clone()), json_to_sea_value(val));
    }
    query.and_where(Expr::col(DynCol("id".into())).eq(id));

    let (sql, values) = crate::render_update(query, backend);
    crate::Statement::new(sql, values, table)
}

/// Build UPDATE {table} SET ... WHERE {filters}.
pub fn build_update_where(
    table: &str,
    data: &[(String, serde_json::Value)],
    filters: &[Filter],
    backend: Backend,
) -> crate::Statement {
    let mut query = Query::update();
    query.table(DynCol(table.into()));

    for (col, val) in data {
        query.value(DynCol(col.clone()), json_to_sea_value(val));
    }
    if let Some(cond) = build_condition(filters) {
        query.cond_where(cond);
    }

    let (sql, values) = crate::render_update(query, backend);
    crate::Statement::new(sql, values, table)
}

/// Build UPDATE {table} SET {col} = {col} + {delta} WHERE {filters}.
///
/// Generates a single-statement atomic increment of a numeric column, so
/// concurrent writers don't race on a read-modify-write of `col`. `delta`
/// is signed — pass a negative value to decrement. `delta = 0` is allowed
/// and produces a no-op UPDATE (callers should normally filter that out,
/// but we don't enforce it here so this stays a pure SQL builder).
///
/// Used by share-link / view-counter / quota-style counters where the new
/// value must be derived from the row's current value rather than supplied
/// by the caller. See `build_update_where` when the new value is a literal.
pub fn build_increment_field_where(
    table: &str,
    col: &str,
    delta: i64,
    filters: &[Filter],
    backend: Backend,
) -> crate::Statement {
    let mut query = Query::update();
    query.table(DynCol(table.into()));

    let col_dyn = DynCol(col.into());
    let increment_expr: SimpleExpr = Expr::col(col_dyn.clone()).add(delta);
    query.value(col_dyn, increment_expr);

    if let Some(cond) = build_condition(filters) {
        query.cond_where(cond);
    }

    let (sql, values) = crate::render_update(query, backend);
    crate::Statement::new(sql, values, table)
}

/// Build DELETE FROM {table} WHERE id = {id}.
pub fn build_delete_by_id(table: &str, id: &str, backend: Backend) -> crate::Statement {
    let mut query = Query::delete();
    query
        .from_table(DynCol(table.into()))
        .and_where(Expr::col(DynCol("id".into())).eq(id));

    let (sql, values) = crate::render_delete(query, backend);
    crate::Statement::new(sql, values, table)
}

/// Build DELETE FROM {table} WHERE {filters}.
pub fn build_delete_where(table: &str, filters: &[Filter], backend: Backend) -> crate::Statement {
    let mut query = Query::delete();
    query.from_table(DynCol(table.into()));

    if let Some(cond) = build_condition(filters) {
        query.cond_where(cond);
    }

    let (sql, values) = crate::render_delete(query, backend);
    crate::Statement::new(sql, values, table)
}

/// Build DELETE FROM {table} WHERE {filters} RETURNING *.
///
/// Sqlite 3.35+ and PostgreSQL both support this form. The caller is
/// responsible for ensuring the backend version requirement is met.
pub fn build_delete_where_returning(
    table: &str,
    filters: &[Filter],
    backend: Backend,
) -> crate::Statement {
    let mut query = Query::delete();
    query.from_table(DynCol(table.into()));

    if let Some(cond) = build_condition(filters) {
        query.cond_where(cond);
    }

    query.returning_all();

    let (sql, values) = crate::render_delete(query, backend);
    crate::Statement::new(sql, values, table)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eq_filter(field: &str, value: serde_json::Value) -> Filter {
        Filter {
            field: field.into(),
            operator: FilterOp::Equal,
            value,
        }
    }

    #[test]
    fn test_build_select_sqlite() {
        let opts = ListOptions {
            filters: vec![eq_filter("name", serde_json::json!("alice"))],
            sort: vec![SortField {
                field: "created_at".into(),
                desc: true,
            }],
            limit: 10,
            offset: 0,
            skip_count: false,
        };
        let stmt = build_select("users", &opts, Backend::Sqlite);
        let sql = stmt.sql;
        let values = stmt.values;
        assert!(sql.contains("SELECT"));
        assert!(sql.contains("FROM"));
        assert!(sql.contains("WHERE"));
        assert!(sql.contains("ORDER BY"));
        assert!(sql.contains("LIMIT"));
        assert!(!values.is_empty()); // filter value + possibly limit
        assert_eq!(stmt.collection, "users");
    }

    #[test]
    fn test_build_select_postgres() {
        let opts = ListOptions {
            filters: vec![eq_filter("name", serde_json::json!("alice"))],
            sort: vec![],
            limit: 0,
            offset: 0,
            skip_count: false,
        };
        let stmt = build_select("users", &opts, Backend::Postgres);
        let sql = stmt.sql;
        let values = stmt.values;
        assert!(sql.contains("$1"));
        assert_eq!(values.len(), 1);
        assert_eq!(stmt.collection, "users");
    }

    #[test]
    fn test_build_insert_sqlite() {
        let data = vec![
            ("id".to_string(), serde_json::json!("abc")),
            ("name".to_string(), serde_json::json!("alice")),
        ];
        let stmt = build_insert("users", &data, Backend::Sqlite);
        let sql = stmt.sql;
        let values = stmt.values;
        assert!(sql.contains("INSERT INTO"));
        assert_eq!(values.len(), 2);
        assert_eq!(stmt.collection, "users");
    }

    #[test]
    fn test_build_update_by_id() {
        let data = vec![("name".to_string(), serde_json::json!("bob"))];
        let stmt = build_update_by_id("users", "123", &data, Backend::Sqlite);
        let sql = stmt.sql;
        let values = stmt.values;
        assert!(sql.contains("UPDATE"));
        assert!(sql.contains("SET"));
        assert!(sql.contains("WHERE"));
        assert_eq!(values.len(), 2); // name + id
        assert_eq!(stmt.collection, "users");
    }

    #[test]
    fn test_build_delete_by_id() {
        let stmt = build_delete_by_id("users", "123", Backend::Sqlite);
        let sql = stmt.sql;
        let values = stmt.values;
        assert!(sql.contains("DELETE FROM"));
        assert!(sql.contains("WHERE"));
        assert_eq!(values.len(), 1);
        assert_eq!(stmt.collection, "users");
    }

    #[test]
    fn test_build_update_where() {
        let data = vec![("status".to_string(), serde_json::json!("active"))];
        let filters = vec![eq_filter("id", serde_json::json!("123"))];
        let stmt = build_update_where("users", &data, &filters, Backend::Postgres);
        let sql = stmt.sql;
        let values = stmt.values;
        assert!(sql.contains("UPDATE"));
        assert!(sql.contains("$1"));
        assert!(sql.contains("$2"));
        assert_eq!(values.len(), 2);
        assert_eq!(stmt.collection, "users");
    }

    #[test]
    fn test_build_delete_where_returning_sqlite() {
        let filters = vec![eq_filter("code_hash", serde_json::json!("abc123"))];
        let stmt = build_delete_where_returning("cli_codes", &filters, Backend::Sqlite);
        let sql = stmt.sql;
        let values = stmt.values;
        assert!(sql.contains("DELETE FROM"));
        assert!(sql.contains("WHERE"));
        assert!(
            sql.contains("RETURNING"),
            "should contain RETURNING clause: {sql}"
        );
        assert_eq!(values.len(), 1);
        assert_eq!(stmt.collection, "cli_codes");
    }

    #[test]
    fn test_build_delete_where_returning_postgres() {
        let filters = vec![eq_filter("code_hash", serde_json::json!("abc123"))];
        let stmt = build_delete_where_returning("cli_codes", &filters, Backend::Postgres);
        let sql = stmt.sql;
        let values = stmt.values;
        assert!(sql.contains("DELETE FROM"));
        assert!(sql.contains("WHERE"));
        assert!(
            sql.contains("RETURNING"),
            "should contain RETURNING clause: {sql}"
        );
        assert!(sql.contains("$1"));
        assert_eq!(values.len(), 1);
        assert_eq!(stmt.collection, "cli_codes");
    }

    #[test]
    fn test_build_delete_where_returning_no_filters() {
        // No filters — should delete all rows and return them
        let stmt = build_delete_where_returning("items", &[], Backend::Sqlite);
        let sql = stmt.sql;
        let values = stmt.values;
        assert!(sql.contains("DELETE FROM"));
        assert!(!sql.contains("WHERE"));
        assert!(sql.contains("RETURNING"));
        assert!(values.is_empty());
        assert_eq!(stmt.collection, "items");
    }

    #[test]
    fn test_build_increment_field_where_sqlite() {
        let filters = vec![eq_filter("id", serde_json::json!("share-abc"))];
        let stmt =
            build_increment_field_where("shares", "access_count", 1, &filters, Backend::Sqlite);
        let sql = stmt.sql;
        let values = stmt.values;
        // SET col = col + ? — the delta is parameter-bound, the column
        // expression is `col + bind` (not `col = bind` — that would clobber).
        assert!(
            sql.contains("UPDATE \"shares\" SET \"access_count\" = \"access_count\" + ?"),
            "expected atomic increment, got: {sql}"
        );
        assert!(sql.contains("WHERE"));
        // Two bindings: delta (1) + filter value.
        assert_eq!(values.len(), 2);
        assert_eq!(stmt.collection, "shares");
    }

    #[test]
    fn test_build_increment_field_where_postgres() {
        let filters = vec![eq_filter("id", serde_json::json!("share-abc"))];
        let stmt =
            build_increment_field_where("shares", "access_count", 1, &filters, Backend::Postgres);
        let sql = stmt.sql;
        let values = stmt.values;
        // Postgres backend renders numbered placeholders.
        assert!(
            sql.contains("UPDATE \"shares\" SET \"access_count\" = \"access_count\" + $1"),
            "expected atomic increment with pg placeholder, got: {sql}"
        );
        assert!(sql.contains("$2"), "expected second pg placeholder: {sql}");
        assert_eq!(values.len(), 2);
        assert_eq!(stmt.collection, "shares");
    }

    #[test]
    fn test_build_increment_field_where_multi_filter() {
        let filters = vec![
            eq_filter("org_id", serde_json::json!("o1")),
            eq_filter("share_token", serde_json::json!("tok")),
        ];
        let stmt =
            build_increment_field_where("shares", "access_count", 1, &filters, Backend::Sqlite);
        let sql = stmt.sql;
        let values = stmt.values;
        assert!(sql.contains("SET \"access_count\" = \"access_count\" + ?"));
        // Two AND-ed filter conditions, both parameterized.
        assert!(sql.contains(" AND "), "expected AND in WHERE: {sql}");
        // delta + two filter values = three bindings.
        assert_eq!(values.len(), 3);
        assert_eq!(stmt.collection, "shares");
    }

    #[test]
    fn test_build_increment_field_where_negative_delta() {
        // Signed delta — decrement is just a negative i64. The delta is
        // bound (not inlined) so the SQL text doesn't distinguish sign;
        // it's the binding value that carries it.
        let filters = vec![eq_filter("id", serde_json::json!("q1"))];
        let stmt = build_increment_field_where(
            "quota_buckets",
            "remaining",
            -5,
            &filters,
            Backend::Sqlite,
        );
        let sql = stmt.sql;
        let values = stmt.values;
        assert!(
            sql.contains("\"remaining\" = \"remaining\" + ?"),
            "expected atomic add expression, got: {sql}"
        );
        // First binding is the delta; verify it's the negative we passed.
        assert_eq!(values[0], sea_query::Value::BigInt(Some(-5)));
        assert_eq!(stmt.collection, "quota_buckets");
    }

    #[test]
    fn test_build_increment_field_where_zero_delta_passthrough() {
        // We accept delta=0 as a no-op UPDATE — callers can filter if they
        // want, but the builder doesn't reject.
        let filters = vec![eq_filter("id", serde_json::json!("x"))];
        let stmt = build_increment_field_where("t", "c", 0, &filters, Backend::Sqlite);
        let sql = stmt.sql;
        let values = stmt.values;
        assert!(sql.contains("SET \"c\" = \"c\" + ?"), "got: {sql}");
        assert_eq!(values[0], sea_query::Value::BigInt(Some(0)));
        assert_eq!(stmt.collection, "t");
    }

    #[test]
    fn in_filter_with_non_array_value_emits_always_false_not_dropped() {
        // Malformed `In` filter (value is a scalar, not an array). The
        // predicate must NOT be dropped — that would widen the result set to
        // every row. Instead we emit an always-false `1=0` so the query
        // returns nothing (narrow, never widen).
        let filters = vec![Filter {
            field: "status".into(),
            operator: FilterOp::In,
            value: serde_json::json!("active"),
        }];
        let cond = build_condition(&filters).expect("non-empty filters yield a condition");
        let stmt = build_delete_where_with(cond);
        assert!(
            stmt.contains("1 = 0") || stmt.contains("1=0"),
            "expected always-false predicate, got: {stmt}"
        );
        // The malformed filter's value must not leak into bindings either.
        assert!(
            !stmt.contains("active"),
            "malformed In value must not become a binding/literal: {stmt}"
        );
    }

    #[test]
    fn like_filter_with_non_string_value_emits_always_false_not_empty_like() {
        // Malformed `Like` filter (value is a number, not a string). Old
        // behaviour coerced this to `LIKE ''`, silently matching only empty
        // strings. Fail-safe behaviour is an always-false predicate.
        let filters = vec![Filter {
            field: "name".into(),
            operator: FilterOp::Like,
            value: serde_json::json!(42),
        }];
        let cond = build_condition(&filters).expect("non-empty filters yield a condition");
        let stmt = build_delete_where_with(cond);
        assert!(
            stmt.contains("1 = 0") || stmt.contains("1=0"),
            "expected always-false predicate, got: {stmt}"
        );
        assert!(
            !stmt.to_uppercase().contains("LIKE"),
            "non-string Like must not render a LIKE clause: {stmt}"
        );
    }

    #[test]
    fn like_filter_with_string_value_still_renders_like() {
        // The happy path must be untouched by the fail-safe coercion.
        let filters = vec![Filter {
            field: "name".into(),
            operator: FilterOp::Like,
            value: serde_json::json!("%alice%"),
        }];
        let cond = build_condition(&filters).expect("non-empty filters yield a condition");
        let stmt = build_delete_where_with(cond);
        assert!(
            stmt.to_uppercase().contains("LIKE"),
            "string Like should render a LIKE clause: {stmt}"
        );
    }

    /// Render a standalone `DELETE FROM t WHERE <cond>` so the fail-safe tests
    /// can inspect the rendered predicate text directly.
    fn build_delete_where_with(cond: Cond) -> String {
        let mut query = Query::delete();
        query.from_table(DynCol("t".into())).cond_where(cond);
        let (sql, _) = crate::render_delete(query, Backend::Sqlite);
        sql
    }

    #[test]
    fn build_select_with_condition_appends_or_group() {
        let or_group = Cond::any()
            .add(Expr::col(DynCol("email".into())).like("%alice%".to_string()))
            .add(Expr::col(DynCol("id".into())).like("%alice%".to_string()));

        let stmt = build_select_with_condition(
            "users",
            &ListOptions {
                filters: vec![Filter {
                    field: "deleted_at".into(),
                    operator: FilterOp::IsNull,
                    value: serde_json::Value::Null,
                }],
                sort: vec![SortField {
                    field: "created_at".into(),
                    desc: true,
                }],
                limit: 20,
                offset: 0,
                ..Default::default()
            },
            Some(or_group),
            Backend::Sqlite,
        );
        let sql = stmt.sql;
        let values = stmt.values;

        assert!(sql.starts_with("SELECT * FROM \"users\""), "got: {sql}");
        assert!(sql.contains("IS NULL"));
        assert!(sql.contains(" OR "), "expected OR in clause, got: {sql}");
        assert!(sql.contains("LIKE"));
        assert!(sql.contains("ORDER BY"));
        assert!(sql.contains("LIMIT"));
        // At least the two LIKE bindings (sea-query may also parameterize
        // LIMIT / OFFSET depending on backend — we don't pin that here).
        assert!(values.len() >= 2, "expected ≥2 bindings, got {values:?}");
        assert_eq!(stmt.collection, "users");
    }
}
