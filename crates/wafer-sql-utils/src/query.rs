use sea_query::{Asterisk, Cond, Expr, Order, Query, SelectStatement, SimpleExpr};
use wafer_core::interfaces::database::service::{Filter, FilterOp, ListOptions, SortField};

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
                    continue;
                }
            }
            FilterOp::Equal => Expr::col(col).eq(json_to_sea_value(&filter.value)),
            FilterOp::NotEqual => Expr::col(col).ne(json_to_sea_value(&filter.value)),
            FilterOp::GreaterThan => Expr::col(col).gt(json_to_sea_value(&filter.value)),
            FilterOp::GreaterEqual => Expr::col(col).gte(json_to_sea_value(&filter.value)),
            FilterOp::LessThan => Expr::col(col).lt(json_to_sea_value(&filter.value)),
            FilterOp::LessEqual => Expr::col(col).lte(json_to_sea_value(&filter.value)),
            FilterOp::Like => Expr::col(col).like(filter.value.as_str().unwrap_or("").to_string()),
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
pub fn build_select(
    table: &str,
    opts: &ListOptions,
    backend: Backend,
) -> (String, Vec<sea_query::Value>) {
    let mut query = Query::select();
    query.column(Asterisk).from(DynCol(table.into()));

    if let Some(cond) = build_condition(&opts.filters) {
        query.cond_where(cond);
    }
    apply_order(&mut query, &opts.sort);
    apply_pagination(&mut query, opts.limit, opts.offset);

    crate::render_select(query, backend)
}

/// Build SELECT {columns} FROM {table} with filters, sort, limit, offset.
/// Also accepts an optional sea-query Cond for complex conditions (OR groups, etc.).
pub fn build_select_columns(
    table: &str,
    columns: &[&str],
    opts: &ListOptions,
    extra_condition: Option<Cond>,
    backend: Backend,
) -> (String, Vec<sea_query::Value>) {
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

    crate::render_select(query, backend)
}

/// Build INSERT INTO {table} (cols) VALUES (vals).
pub fn build_insert(
    table: &str,
    data: &[(String, serde_json::Value)],
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

    crate::render_insert(query, backend)
}

/// Build UPDATE {table} SET ... WHERE id = {id}.
pub fn build_update_by_id(
    table: &str,
    id: &str,
    data: &[(String, serde_json::Value)],
    backend: Backend,
) -> (String, Vec<sea_query::Value>) {
    let mut query = Query::update();
    query.table(DynCol(table.into()));

    for (col, val) in data {
        query.value(DynCol(col.clone()), json_to_sea_value(val));
    }
    query.and_where(Expr::col(DynCol("id".into())).eq(id));

    crate::render_update(query, backend)
}

/// Build UPDATE {table} SET ... WHERE {filters}.
pub fn build_update_where(
    table: &str,
    data: &[(String, serde_json::Value)],
    filters: &[Filter],
    backend: Backend,
) -> (String, Vec<sea_query::Value>) {
    let mut query = Query::update();
    query.table(DynCol(table.into()));

    for (col, val) in data {
        query.value(DynCol(col.clone()), json_to_sea_value(val));
    }
    if let Some(cond) = build_condition(filters) {
        query.cond_where(cond);
    }

    crate::render_update(query, backend)
}

/// Build DELETE FROM {table} WHERE id = {id}.
pub fn build_delete_by_id(
    table: &str,
    id: &str,
    backend: Backend,
) -> (String, Vec<sea_query::Value>) {
    let mut query = Query::delete();
    query
        .from_table(DynCol(table.into()))
        .and_where(Expr::col(DynCol("id".into())).eq(id));

    crate::render_delete(query, backend)
}

/// Build DELETE FROM {table} WHERE {filters}.
pub fn build_delete_where(
    table: &str,
    filters: &[Filter],
    backend: Backend,
) -> (String, Vec<sea_query::Value>) {
    let mut query = Query::delete();
    query.from_table(DynCol(table.into()));

    if let Some(cond) = build_condition(filters) {
        query.cond_where(cond);
    }

    crate::render_delete(query, backend)
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
        };
        let (sql, values) = build_select("users", &opts, Backend::Sqlite);
        assert!(sql.contains("SELECT"));
        assert!(sql.contains("FROM"));
        assert!(sql.contains("WHERE"));
        assert!(sql.contains("ORDER BY"));
        assert!(sql.contains("LIMIT"));
        assert!(!values.is_empty()); // filter value + possibly limit
    }

    #[test]
    fn test_build_select_postgres() {
        let opts = ListOptions {
            filters: vec![eq_filter("name", serde_json::json!("alice"))],
            sort: vec![],
            limit: 0,
            offset: 0,
        };
        let (sql, values) = build_select("users", &opts, Backend::Postgres);
        assert!(sql.contains("$1"));
        assert_eq!(values.len(), 1);
    }

    #[test]
    fn test_build_insert_sqlite() {
        let data = vec![
            ("id".to_string(), serde_json::json!("abc")),
            ("name".to_string(), serde_json::json!("alice")),
        ];
        let (sql, values) = build_insert("users", &data, Backend::Sqlite);
        assert!(sql.contains("INSERT INTO"));
        assert_eq!(values.len(), 2);
    }

    #[test]
    fn test_build_update_by_id() {
        let data = vec![("name".to_string(), serde_json::json!("bob"))];
        let (sql, values) = build_update_by_id("users", "123", &data, Backend::Sqlite);
        assert!(sql.contains("UPDATE"));
        assert!(sql.contains("SET"));
        assert!(sql.contains("WHERE"));
        assert_eq!(values.len(), 2); // name + id
    }

    #[test]
    fn test_build_delete_by_id() {
        let (sql, values) = build_delete_by_id("users", "123", Backend::Sqlite);
        assert!(sql.contains("DELETE FROM"));
        assert!(sql.contains("WHERE"));
        assert_eq!(values.len(), 1);
    }

    #[test]
    fn test_build_update_where() {
        let data = vec![("status".to_string(), serde_json::json!("active"))];
        let filters = vec![eq_filter("id", serde_json::json!("123"))];
        let (sql, values) = build_update_where("users", &data, &filters, Backend::Postgres);
        assert!(sql.contains("UPDATE"));
        assert!(sql.contains("$1"));
        assert!(sql.contains("$2"));
        assert_eq!(values.len(), 2);
    }
}
