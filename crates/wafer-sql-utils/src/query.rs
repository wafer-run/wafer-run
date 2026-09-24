use sea_query::{
    Asterisk, Cond, Expr, InsertStatement, LikeExpr, Order, Query, SelectStatement, SimpleExpr,
    UpdateStatement,
};
use wafer_block::db::{
    ColumnCompareOp, ColumnFilter, Filter, FilterOp, FilterTree, ListOptions, SortField,
};

use crate::{ident::DynCol, value::json_to_sea_value, Backend, SqlBuildError};

/// Render one [`Filter`] leaf to a sea-query predicate expression.
///
/// Field names reach sea-query via [`DynCol`], which quotes them, so this is
/// injection-safe without a separate identifier validation step. Malformed
/// operand shapes (`In` with a non-array, `Like` with a non-string) render an
/// always-false `1=0` predicate — narrow, never widen (see the inline notes).
pub(crate) fn leaf_expr(filter: &Filter) -> SimpleExpr {
    let col = DynCol(filter.field.clone());
    match filter.operator {
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
                // Explicit ESCAPE clause: SQLite/D1's LIKE has no default
                // escape character, so without this a caller's
                // backslash-escaped wildcards (`\%`, `\_`, `\\`) render as
                // inert literal backslashes and the escaping silently does
                // nothing. `\` is one well-defined escape char on both
                // backends — mirrors `vector::build_list_meta_tables`, which
                // appends the same `ESCAPE '\'` by hand for its raw-SQL path.
                Expr::col(col).like(LikeExpr::new(pattern).escape('\\'))
            } else {
                // Fail-safe: a `Like` filter whose value isn't a JSON
                // string is malformed input. Emit an always-false predicate
                // rather than coercing to `LIKE ''` (which matches only
                // empty strings — a surprising, non-failing result). Same
                // narrow-never-widen rule as the `In` arm above.
                Expr::cust("1=0")
            }
        }
    }
}

/// Render one [`ColumnFilter`] leaf — `field <op> column` — to a sea-query
/// predicate. Both columns reach sea-query via [`DynCol`], which quotes them.
fn column_compare_expr(filter: &ColumnFilter) -> SimpleExpr {
    let left = Expr::col(DynCol(filter.field.clone()));
    let right = Expr::col(DynCol(filter.column.clone()));
    match filter.operator {
        ColumnCompareOp::Equal => left.eq(right),
        ColumnCompareOp::NotEqual => left.ne(right),
        ColumnCompareOp::GreaterThan => left.gt(right),
        ColumnCompareOp::GreaterEqual => left.gte(right),
        ColumnCompareOp::LessThan => left.lt(right),
        ColumnCompareOp::LessEqual => left.lte(right),
    }
}

/// Convert a slice of Filters into a sea_query Cond (AND-combined).
/// Returns None if filters is empty.
pub fn build_condition(filters: &[Filter]) -> Option<Cond> {
    if filters.is_empty() {
        return None;
    }
    let mut cond = Cond::all();
    for filter in filters {
        cond = cond.add(leaf_expr(filter));
    }
    Some(cond)
}

/// Convert a predicate **tree** into a sea_query `Cond`. The top-level slice
/// is AND-combined; `All` nodes render `Cond::all()`, `Any` nodes
/// `Cond::any()`, value leaves render via [`leaf_expr`] and column-to-column
/// leaves as `field <op> column`. Empty slice → `None`.
///
/// Bounds (depth / node count) are enforced by the caller (the database
/// handler) before conversion, so this function assumes already-validated
/// input and cannot itself fail.
pub fn build_condition_tree(nodes: &[FilterTree]) -> Option<Cond> {
    if nodes.is_empty() {
        return None;
    }
    let mut cond = Cond::all();
    for node in nodes {
        cond = cond.add(node_to_cond(node));
    }
    Some(cond)
}

/// Convert a predicate **tree** into a single boolean [`SimpleExpr`] — the
/// same AND/OR structure as [`build_condition_tree`], but as an expression
/// usable outside a `WHERE` clause: most notably the predicate of a
/// `SUM(CASE WHEN <expr> THEN 1 ELSE 0 END)` conditional count built via
/// [`crate::aggregate::AggregateColumn::case_when_sum`].
///
/// The conversion goes through [`build_condition_tree`] and sea-query's
/// `From<Condition> for SimpleExpr`, which folds the tree with `.and()` /
/// `.or()` exactly as the `WHERE`-clause path does — so a `WHERE` and a
/// `CASE WHEN` built from the same tree render identical predicates. An empty
/// forest folds to an always-true constant (an empty `AND`); callers that
/// require a non-empty predicate (the aggregate handler) reject empty input
/// upstream. Bounds are enforced before conversion, so this cannot fail.
#[must_use]
pub fn tree_to_simple_expr(nodes: &[FilterTree]) -> SimpleExpr {
    SimpleExpr::from(build_condition_tree(nodes).unwrap_or_else(Cond::all))
}

fn node_to_cond(node: &FilterTree) -> Cond {
    match node {
        FilterTree::Leaf(f) => Cond::all().add(leaf_expr(f)),
        FilterTree::ColumnCompare(f) => Cond::all().add(column_compare_expr(f)),
        FilterTree::All(children) => {
            let mut c = Cond::all();
            for child in children {
                c = c.add(node_to_cond(child));
            }
            c
        }
        FilterTree::Any(children) => {
            let mut c = Cond::any();
            for child in children {
                c = c.add(node_to_cond(child));
            }
            c
        }
    }
}

/// Apply sort directives to a SelectStatement.
///
/// Emits exactly `sort`, nothing more — rows that tie on every sort key come
/// back in whatever order the backend produces. That is what a grouped
/// aggregate needs (its sort keys are group columns and aliases, and a column
/// outside the `GROUP BY` is not a valid sort key there), and it is why the
/// row-returning selects go through [`apply_order_with_unique_key`] instead.
pub fn apply_order(query: &mut SelectStatement, sort: &[SortField]) {
    for s in sort {
        let order = if s.desc { Order::Desc } else { Order::Asc };
        query.order_by(DynCol(s.field.clone()), order);
    }
}

/// Whether the row select for `opts` has an `ORDER BY` — it sorts, or it
/// pages with `limit`/`offset` — and therefore orders by the table's unique
/// key as its final terms (see [`apply_order_with_unique_key`]).
///
/// An executor looks the unique key up only when this is `true`, so an
/// unsorted, unpaged read pays for no introspection.
#[must_use]
pub fn orders_rows(opts: &ListOptions) -> bool {
    !opts.sort.is_empty() || opts.limit.is_some() || opts.offset > 0
}

/// Apply `sort`, then the columns of `unique_key` that `sort` does not already
/// name, so rows that tie on every sort key still come back in one order that
/// is the same on every query — which is what makes `limit`/`offset` pages
/// disjoint and complete.
///
/// `unique_key` is the table's primary key (all of its columns, in key
/// order). Its columns take the direction of the last sort term, so a
/// newest-first list breaks ties newest-key-first; with no sort they are
/// ascending. Nothing is emitted unless [`orders_rows`] holds — an unsorted,
/// unpaged select stays unordered. An empty `unique_key` (a table with no
/// primary key) appends nothing: such a table has no column set that
/// identifies a row, so its ties stay backend-ordered.
pub fn apply_order_with_unique_key(
    query: &mut SelectStatement,
    opts: &ListOptions,
    unique_key: &[&str],
) {
    if !orders_rows(opts) {
        return;
    }
    apply_order(query, &opts.sort);
    let order = if opts.sort.last().is_some_and(|s| s.desc) {
        Order::Desc
    } else {
        Order::Asc
    };
    for col in unique_key {
        if opts.sort.iter().any(|s| s.field == *col) {
            continue;
        }
        query.order_by(DynCol((*col).to_string()), order.clone());
    }
}

/// Check that `limit`/`offset` can be rendered: a zero limit is
/// [`SqlBuildError::ZeroLimit`] and a positive offset without a limit is
/// [`SqlBuildError::OffsetWithoutLimit`] (SQLite and D1 have no `OFFSET`
/// without `LIMIT`). [`apply_pagination`] applies the same check; an
/// executor that can answer without rendering a select (a missing table)
/// calls this first so it refuses the same requests.
pub fn check_pagination(limit: Option<u32>, offset: i64) -> Result<(), SqlBuildError> {
    match limit {
        Some(0) => Err(SqlBuildError::ZeroLimit),
        None if offset > 0 => Err(SqlBuildError::OffsetWithoutLimit { offset }),
        _ => Ok(()),
    }
}

/// Apply limit and offset to a SelectStatement, after [`check_pagination`].
///
/// `None` emits no `LIMIT`, so every matching row comes back. An offset of 0
/// or less emits no `OFFSET`.
pub fn apply_pagination(
    query: &mut SelectStatement,
    limit: Option<u32>,
    offset: i64,
) -> Result<(), SqlBuildError> {
    check_pagination(limit, offset)?;
    if let Some(n) = limit {
        query.limit(u64::from(n));
    }
    if offset > 0 {
        query.offset(offset as u64);
    }
    Ok(())
}

/// Build SELECT * FROM {table} with filters, sort, limit, offset.
///
/// `unique_key` is the table's primary key, appended to the `ORDER BY` so
/// ties resolve the same way on every query (see
/// [`apply_order_with_unique_key`]); pass `&[]` for a table that has none.
/// Fails when `opts.limit`/`opts.offset` cannot be rendered (see
/// [`apply_pagination`]).
pub fn build_select(
    table: &str,
    opts: &ListOptions,
    unique_key: &[&str],
    backend: Backend,
) -> Result<crate::Statement, SqlBuildError> {
    build_select_with_condition(table, opts, None, unique_key, backend)
}

/// Build SELECT * FROM {table} with filters, sort, limit, offset, plus an
/// optional extra sea-query `Cond` AND-ed with the filters clause.
///
/// Use this when you need a complex WHERE — most commonly an OR group —
/// alongside the flat AND-of-filters list, without giving up `SELECT *`.
/// See [`build_select_columns`] for the projection variant and an OR-group
/// example, and [`build_select`] for `unique_key`.
pub fn build_select_with_condition(
    table: &str,
    opts: &ListOptions,
    extra_condition: Option<Cond>,
    unique_key: &[&str],
    backend: Backend,
) -> Result<crate::Statement, SqlBuildError> {
    select_with_projection(table, None, opts, extra_condition, unique_key, backend)
}

/// Shared body of [`build_select_with_condition`] (`None` projection →
/// `SELECT *`) and [`build_select_columns`] (explicit column list).
fn select_with_projection(
    table: &str,
    columns: Option<&[&str]>,
    opts: &ListOptions,
    extra_condition: Option<Cond>,
    unique_key: &[&str],
    backend: Backend,
) -> Result<crate::Statement, SqlBuildError> {
    let mut query = Query::select();
    match columns {
        Some(cols) => {
            for col in cols {
                query.column(DynCol((*col).into()));
            }
        }
        None => {
            query.column(Asterisk);
        }
    }
    query.from(DynCol(table.into()));

    if let Some(cond) = build_condition(&opts.filters) {
        query.cond_where(cond);
    }
    if let Some(extra) = extra_condition {
        query.cond_where(extra);
    }
    apply_order_with_unique_key(&mut query, opts, unique_key);
    apply_pagination(&mut query, opts.limit, opts.offset)?;

    let (sql, values) = crate::render_select(query, backend);
    Ok(crate::Statement::new(sql, values, table))
}

/// Build SELECT {columns} FROM {table} with filters, sort, limit, offset.
///
/// `extra_condition` is a sea-query `Cond` that is AND-ed with the
/// `opts.filters` clause. `unique_key` is as for [`build_select`]. Use
/// `extra_condition` for conditions that don't fit the flat AND-of-filters
/// model — for example, an OR group:
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
///     &["id"],
///     Backend::Sqlite,
/// )?;
/// ```
pub fn build_select_columns(
    table: &str,
    columns: &[&str],
    opts: &ListOptions,
    extra_condition: Option<Cond>,
    unique_key: &[&str],
    backend: Backend,
) -> Result<crate::Statement, SqlBuildError> {
    select_with_projection(
        table,
        Some(columns),
        opts,
        extra_condition,
        unique_key,
        backend,
    )
}

/// The `INSERT INTO {table} (cols) VALUES (vals)` both insert builders render.
fn insert_query(table: &str, data: &[(String, serde_json::Value)]) -> InsertStatement {
    let mut query = Query::insert();
    query.into_table(DynCol(table.into()));

    let cols: Vec<DynCol> = data.iter().map(|(k, _)| DynCol(k.clone())).collect();
    let vals: Vec<SimpleExpr> = data
        .iter()
        .map(|(_, v)| json_to_sea_value(v).into())
        .collect();

    query.columns(cols);
    query.values_panic(vals);
    query
}

/// Build INSERT INTO {table} (cols) VALUES (vals).
pub fn build_insert(
    table: &str,
    data: &[(String, serde_json::Value)],
    backend: Backend,
) -> crate::Statement {
    let (sql, values) = crate::render_insert(insert_query(table, data), backend);
    crate::Statement::new(sql, values, table)
}

/// Build INSERT INTO {table} (cols) VALUES (vals) RETURNING *.
///
/// The row comes back as the database stored it, including a key the table
/// generated itself (a SQLite rowid alias, a Postgres identity or `SERIAL`
/// column).
pub fn build_insert_returning(
    table: &str,
    data: &[(String, serde_json::Value)],
    backend: Backend,
) -> crate::Statement {
    let mut query = insert_query(table, data);
    query.returning_all();
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

/// The `UPDATE {table} SET ... WHERE id = {id}` both by-id update builders
/// render.
fn update_by_id_query(
    table: &str,
    id: &str,
    data: &[(String, serde_json::Value)],
) -> UpdateStatement {
    let mut query = Query::update();
    query.table(DynCol(table.into()));

    for (col, val) in data {
        query.value(DynCol(col.clone()), json_to_sea_value(val));
    }
    query.and_where(Expr::col(DynCol("id".into())).eq(id));
    query
}

/// Build UPDATE {table} SET ... WHERE id = {id}.
pub fn build_update_by_id(
    table: &str,
    id: &str,
    data: &[(String, serde_json::Value)],
    backend: Backend,
) -> crate::Statement {
    let (sql, values) = crate::render_update(update_by_id_query(table, id, data), backend);
    crate::Statement::new(sql, values, table)
}

/// Build UPDATE {table} SET ... WHERE id = {id} RETURNING *.
///
/// Returns the updated row, or no row when `id` matched nothing.
pub fn build_update_by_id_returning(
    table: &str,
    id: &str,
    data: &[(String, serde_json::Value)],
    backend: Backend,
) -> crate::Statement {
    let mut query = update_by_id_query(table, id, data);
    query.returning_all();
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
            limit: Some(10),
            offset: 0,
            skip_count: false,
            filter_tree: None,
            columns: None,
        };
        let stmt = build_select("users", &opts, &["id"], Backend::Sqlite).expect("renders");
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
            limit: None,
            offset: 0,
            skip_count: false,
            filter_tree: None,
            columns: None,
        };
        let stmt = build_select("users", &opts, &["id"], Backend::Postgres).expect("renders");
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
    fn insert_returning_renders_returning_on_both_backends() {
        let data = vec![("name".to_string(), serde_json::json!("a"))];
        for backend in [Backend::Sqlite, Backend::Postgres] {
            let stmt = build_insert_returning("items", &data, backend);
            assert!(stmt.sql.starts_with("INSERT INTO"), "{}", stmt.sql);
            assert!(stmt.sql.ends_with("RETURNING *"), "{}", stmt.sql);
            assert_eq!(stmt.values.len(), 1);
            let plain = build_insert("items", &data, backend);
            assert!(!plain.sql.contains("RETURNING"), "{}", plain.sql);
        }
    }

    #[test]
    fn update_by_id_returning_renders_returning_on_both_backends() {
        let data = vec![("name".to_string(), serde_json::json!("a"))];
        for backend in [Backend::Sqlite, Backend::Postgres] {
            let stmt = build_update_by_id_returning("items", "i1", &data, backend);
            assert!(stmt.sql.starts_with("UPDATE"), "{}", stmt.sql);
            assert!(stmt.sql.contains("WHERE"), "{}", stmt.sql);
            assert!(stmt.sql.ends_with("RETURNING *"), "{}", stmt.sql);
            assert_eq!(stmt.values.len(), 2, "the SET value and the id");
            let plain = build_update_by_id("items", "i1", &data, backend);
            assert!(!plain.sql.contains("RETURNING"), "{}", plain.sql);
        }
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

    #[test]
    fn like_filter_renders_explicit_escape_clause() {
        // SQLite/D1 LIKE has no default escape character, so a caller's
        // backslash-escaped wildcards (`\%`, `\_`, `\\`) are inert unless the
        // rendered SQL carries an explicit ESCAPE clause. Assert on the
        // literal clause text, not just presence of "LIKE".
        let filters = vec![Filter {
            field: "name".into(),
            operator: FilterOp::Like,
            value: serde_json::json!("a\\_b"),
        }];
        let cond = build_condition(&filters).expect("non-empty filters yield a condition");
        let stmt = build_delete_where_with(cond);
        assert!(
            stmt.contains("ESCAPE '\\'"),
            "LIKE must emit ESCAPE '\\' so backslash-escaping works on SQLite/D1: {stmt}"
        );
    }

    #[test]
    fn like_filter_renders_escape_clause_on_postgres_too() {
        // One escaping contract on both backends — not just SQLite. Postgres
        // renders backslash-containing literals as `E'...'` (its standard
        // escape-string syntax), so the clause is `ESCAPE E'\\'` rather than
        // SQLite's `ESCAPE '\'` — both are valid SQL for "escape char is a
        // single backslash" on their respective backend.
        let filters = vec![Filter {
            field: "name".into(),
            operator: FilterOp::Like,
            value: serde_json::json!("a\\_b"),
        }];
        let cond = build_condition(&filters).expect("non-empty filters yield a condition");
        let mut query = Query::delete();
        query.from_table(DynCol("t".into())).cond_where(cond);
        let (sql, _) = crate::render_delete(query, Backend::Postgres);
        assert!(
            sql.contains("ESCAPE E'\\\\'"),
            "Postgres LIKE must also emit an explicit ESCAPE clause: {sql}"
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
                limit: Some(20),
                offset: 0,
                ..Default::default()
            },
            Some(or_group),
            &["id"],
            Backend::Sqlite,
        )
        .expect("renders");
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

    #[test]
    fn build_condition_tree_empty_is_none() {
        assert!(build_condition_tree(&[]).is_none());
    }

    #[test]
    fn build_condition_tree_flat_leaves_are_anded() {
        use wafer_block::db::{Filter, FilterOp, FilterTree};
        let tree = vec![
            FilterTree::Leaf(Filter {
                field: "status".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!("active"),
            }),
            FilterTree::Leaf(Filter {
                field: "age".into(),
                operator: FilterOp::GreaterThan,
                value: serde_json::json!(18),
            }),
        ];
        let cond = build_condition_tree(&tree).expect("some");
        let mut q = sea_query::Query::select();
        q.column(sea_query::Asterisk)
            .from(crate::ident::DynCol("t".into()))
            .cond_where(cond);
        let (sql, _) = crate::render_select(q, Backend::Sqlite);
        assert!(sql.contains("\"status\""), "{sql}");
        assert!(sql.contains("AND"), "{sql}");
    }

    #[test]
    fn build_condition_tree_any_group_renders_or() {
        use wafer_block::db::{Filter, FilterOp, FilterTree};
        let tree = vec![FilterTree::Any(vec![
            FilterTree::Leaf(Filter {
                field: "email".into(),
                operator: FilterOp::Like,
                value: serde_json::json!("%a%"),
            }),
            FilterTree::Leaf(Filter {
                field: "id".into(),
                operator: FilterOp::Like,
                value: serde_json::json!("%a%"),
            }),
        ])];
        let cond = build_condition_tree(&tree).expect("some");
        let mut q = sea_query::Query::select();
        q.column(sea_query::Asterisk)
            .from(crate::ident::DynCol("t".into()))
            .cond_where(cond);
        let (sql, _) = crate::render_select(q, Backend::Sqlite);
        assert!(sql.contains(" OR "), "{sql}");
    }

    #[test]
    fn tree_to_simple_expr_leaf_renders_predicate() {
        use wafer_block::db::{Filter, FilterOp, FilterTree};
        let tree = vec![FilterTree::Leaf(Filter {
            field: "status".into(),
            operator: FilterOp::GreaterEqual,
            value: serde_json::json!(400),
        })];
        let expr = tree_to_simple_expr(&tree);
        let mut q = sea_query::Query::select();
        q.expr(expr).from(DynCol("t".into()));
        let (sql, _) = crate::render_select(q, Backend::Sqlite);
        assert!(sql.contains("\"status\""), "{sql}");
        assert!(sql.contains(">="), "{sql}");
    }

    #[test]
    fn tree_to_simple_expr_any_group_renders_or() {
        use wafer_block::db::{Filter, FilterOp, FilterTree};
        let tree = vec![FilterTree::Any(vec![
            FilterTree::Leaf(Filter {
                field: "a".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!(1),
            }),
            FilterTree::Leaf(Filter {
                field: "b".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!(2),
            }),
        ])];
        let expr = tree_to_simple_expr(&tree);
        let mut q = sea_query::Query::select();
        q.expr(expr).from(DynCol("t".into()));
        let (sql, _) = crate::render_select(q, Backend::Sqlite);
        assert!(sql.contains(" OR "), "{sql}");
    }

    #[test]
    fn tree_to_simple_expr_empty_folds_to_constant_and_does_not_panic() {
        // Empty forest → always-true constant (empty AND). Callers reject
        // empty upstream; this only guarantees totality.
        let expr = tree_to_simple_expr(&[]);
        let mut q = sea_query::Query::select();
        q.expr(expr).from(DynCol("t".into()));
        let (sql, _) = crate::render_select(q, Backend::Sqlite);
        assert!(sql.starts_with("SELECT"), "{sql}");
    }

    #[test]
    fn build_condition_tree_nested_all_of_any() {
        use wafer_block::db::{Filter, FilterOp, FilterTree};
        let tree = vec![
            FilterTree::Leaf(Filter {
                field: "active".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!(true),
            }),
            FilterTree::Any(vec![
                FilterTree::Leaf(Filter {
                    field: "role".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!("admin"),
                }),
                FilterTree::Leaf(Filter {
                    field: "role".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!("owner"),
                }),
            ]),
        ];
        let cond = build_condition_tree(&tree).expect("some");
        let mut q = sea_query::Query::select();
        q.column(sea_query::Asterisk)
            .from(crate::ident::DynCol("t".into()))
            .cond_where(cond);
        let (sql, _) = crate::render_select(q, Backend::Sqlite);
        assert!(sql.contains(" OR "), "{sql}");
        assert!(sql.contains("AND"), "{sql}");
    }

    #[test]
    fn column_compare_leaf_renders_both_sides_as_quoted_columns() {
        use wafer_block::db::{ColumnCompareOp, ColumnFilter, FilterTree};
        let cases = [
            (ColumnCompareOp::Equal, "="),
            (ColumnCompareOp::NotEqual, "<>"),
            (ColumnCompareOp::GreaterThan, ">"),
            (ColumnCompareOp::GreaterEqual, ">="),
            (ColumnCompareOp::LessThan, "<"),
            (ColumnCompareOp::LessEqual, "<="),
        ];
        for backend in [Backend::Sqlite, Backend::Postgres] {
            for (operator, sql_op) in cases {
                let tree = vec![FilterTree::ColumnCompare(ColumnFilter {
                    field: "refunded_total_cents".into(),
                    operator,
                    column: "total_cents".into(),
                })];
                let mut q = sea_query::Query::select();
                q.column(sea_query::Asterisk)
                    .from(crate::ident::DynCol("orders".into()))
                    .cond_where(build_condition_tree(&tree).expect("some"));
                let (sql, values) = crate::render_select(q, backend);
                assert_eq!(
                    sql,
                    format!(
                        "SELECT * FROM \"orders\" WHERE \"refunded_total_cents\" {sql_op} \"total_cents\""
                    ),
                    "{backend:?} {operator:?}"
                );
                assert!(values.is_empty(), "a column operand binds no value");
            }
        }
    }

    fn sorted(sort: &[(&str, bool)], limit: Option<u32>, offset: i64) -> ListOptions {
        ListOptions {
            sort: sort
                .iter()
                .map(|(field, desc)| SortField {
                    field: (*field).into(),
                    desc: *desc,
                })
                .collect(),
            limit,
            offset,
            ..Default::default()
        }
    }

    #[test]
    fn a_sorted_select_ends_with_the_unique_key_in_the_last_sort_direction() {
        let opts = sorted(&[("created_at", true)], None, 0);
        let sqlite = build_select("t", &opts, &["id"], Backend::Sqlite)
            .expect("renders")
            .sql;
        assert_eq!(
            sqlite,
            r#"SELECT * FROM "t" ORDER BY "created_at" DESC, "id" DESC"#
        );
        let postgres = build_select("t", &opts, &["id"], Backend::Postgres)
            .expect("renders")
            .sql;
        assert_eq!(
            postgres,
            r#"SELECT * FROM "t" ORDER BY "created_at" DESC, "id" DESC"#
        );
        let asc = build_select(
            "t",
            &sorted(&[("created_at", false)], None, 0),
            &["id"],
            Backend::Sqlite,
        )
        .expect("renders")
        .sql;
        assert_eq!(
            asc,
            r#"SELECT * FROM "t" ORDER BY "created_at" ASC, "id" ASC"#
        );
    }

    #[test]
    fn a_paged_select_with_no_sort_orders_by_the_unique_key() {
        for (limit, offset) in [(Some(2), 0), (Some(2), 4)] {
            let sql = build_select("t", &sorted(&[], limit, offset), &["id"], Backend::Sqlite)
                .expect("renders")
                .sql;
            assert!(
                sql.starts_with(r#"SELECT * FROM "t" ORDER BY "id" ASC"#),
                "limit {limit:?} offset {offset}: {sql}"
            );
        }
        // Neither sorted nor paged: no ORDER BY at all.
        let sql = build_select("t", &sorted(&[], None, 0), &["id"], Backend::Sqlite)
            .expect("renders")
            .sql;
        assert_eq!(sql, r#"SELECT * FROM "t""#);
    }

    /// SQLite has no `OFFSET` without `LIMIT`: the rendered statement would be
    /// a syntax error there and a whole-table read after the offset on
    /// Postgres. Both backends refuse it instead.
    #[test]
    fn an_offset_without_a_limit_is_refused_on_every_backend() {
        for backend in [Backend::Sqlite, Backend::Postgres] {
            let err = build_select("t", &sorted(&[], None, 1), &["id"], backend)
                .expect_err("offset without limit");
            assert_eq!(
                err,
                SqlBuildError::OffsetWithoutLimit { offset: 1 },
                "{backend:?}"
            );
        }
    }

    /// `Some(0)` is a page size nobody set, not a request for an empty page;
    /// "every row" is `None`.
    #[test]
    fn a_zero_limit_is_refused_and_no_limit_emits_no_limit_clause() {
        for backend in [Backend::Sqlite, Backend::Postgres] {
            let err = build_select("t", &sorted(&[], Some(0), 0), &["id"], backend)
                .expect_err("zero limit");
            assert_eq!(err, SqlBuildError::ZeroLimit, "{backend:?}");
            let sql = build_select("t", &sorted(&[("id", false)], None, 0), &["id"], backend)
                .expect("renders")
                .sql;
            assert!(!sql.contains("LIMIT"), "{backend:?}: {sql}");
        }
    }

    #[test]
    fn the_unique_key_is_not_repeated_and_every_key_column_is_appended() {
        let sql = build_select(
            "t",
            &sorted(&[("id", true)], Some(5), 0),
            &["id"],
            Backend::Sqlite,
        )
        .expect("renders")
        .sql;
        assert!(
            sql.starts_with(r#"SELECT * FROM "t" ORDER BY "id" DESC LIMIT"#),
            "{sql}"
        );
        // A composite key: both columns, key order, minus any the sort names.
        let composite = &["user_id", "role_id"];
        let sql = build_select(
            "t",
            &sorted(&[("created_at", false)], None, 0),
            composite,
            Backend::Postgres,
        )
        .expect("renders")
        .sql;
        assert_eq!(
            sql,
            r#"SELECT * FROM "t" ORDER BY "created_at" ASC, "user_id" ASC, "role_id" ASC"#
        );
        let sql = build_select(
            "t",
            &sorted(&[("role_id", true)], None, 0),
            composite,
            Backend::Postgres,
        )
        .expect("renders")
        .sql;
        assert_eq!(
            sql,
            r#"SELECT * FROM "t" ORDER BY "role_id" DESC, "user_id" DESC"#
        );
        // No primary key: the sort alone.
        let sql = build_select(
            "t",
            &sorted(&[("created_at", true)], None, 0),
            &[],
            Backend::Sqlite,
        )
        .expect("renders")
        .sql;
        assert_eq!(sql, r#"SELECT * FROM "t" ORDER BY "created_at" DESC"#);
        // A projection is ordered the same way.
        let sql = build_select_columns(
            "t",
            &["name"],
            &sorted(&[("name", false)], None, 0),
            None,
            &["id"],
            Backend::Sqlite,
        )
        .expect("renders")
        .sql;
        assert_eq!(
            sql,
            r#"SELECT "name" FROM "t" ORDER BY "name" ASC, "id" ASC"#
        );
    }

    /// A grouped aggregate shares `apply_order` with the row select but must
    /// not gain a key column: `id` is not a `GROUP BY` term, which Postgres
    /// rejects. Passes before and after the unique-key change by design.
    #[test]
    fn a_grouped_aggregate_orders_by_its_sort_alone() {
        let stmt = crate::aggregate::build_grouped_query(
            crate::aggregate::GroupedQueryConfig {
                table: "t".into(),
                select_columns: vec!["category".into()],
                aggregates: vec![crate::aggregate::AggregateColumn {
                    func: crate::aggregate::AggFunc::Count,
                    field: None,
                    alias: "cnt".into(),
                    cast_as: None,
                    inner_expr: None,
                }],
                filters: vec![],
                group_by: vec!["category".into()],
                date_buckets: vec![],
                order_by: vec![SortField {
                    field: "cnt".into(),
                    desc: true,
                }],
                limit: Some(5),
            },
            Backend::Postgres,
        )
        .expect("renders");
        assert!(
            stmt.sql.ends_with(r#"ORDER BY "cnt" DESC LIMIT $1"#),
            "{}",
            stmt.sql
        );
    }

    /// Five rows tie on the sort key and were inserted out of key order; pages
    /// of two must be disjoint, complete, and in key order — run on a real
    /// SQLite engine, which returns ties in insertion order when nothing
    /// breaks them.
    #[test]
    fn pages_over_a_tied_sort_key_are_disjoint_and_complete_in_sqlite() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (id TEXT PRIMARY KEY, created_at TEXT)", [])
            .unwrap();
        for id in ["c", "a", "e", "b", "d"] {
            conn.execute(
                "INSERT INTO t (id, created_at) VALUES (?1, '2026-01-01')",
                [id],
            )
            .unwrap();
        }
        let mut seen = Vec::new();
        for offset in [0, 2, 4] {
            let stmt = build_select(
                "t",
                &sorted(&[("created_at", true)], Some(2), offset),
                &["id"],
                Backend::Sqlite,
            )
            .expect("renders");
            let params: Vec<i64> = stmt
                .values
                .iter()
                .map(|v| match v {
                    sea_query::Value::BigUnsigned(Some(n)) => i64::try_from(*n).unwrap(),
                    sea_query::Value::BigInt(Some(n)) => *n,
                    other => panic!("unexpected bound value {other:?}"),
                })
                .collect();
            let mut q = conn.prepare(&stmt.sql).unwrap();
            let ids: Vec<String> = q
                .query_map(rusqlite::params_from_iter(params), |r| r.get("id"))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            seen.push(ids);
        }
        assert_eq!(
            seen,
            vec![vec!["e", "d"], vec!["c", "b"], vec!["a"]],
            "newest-first ties break id-descending"
        );
    }
}
