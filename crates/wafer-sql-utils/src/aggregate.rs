use sea_query::{Alias, Asterisk, Expr, Func, Query, SimpleExpr};
use wafer_core::interfaces::database::service::{Filter, SortField};

use crate::{
    ident::DynCol,
    query::{apply_order, build_condition},
    Backend,
};

/// Build SELECT COUNT(*) FROM {table} WHERE {filters}.
pub fn build_count(
    table: &str,
    filters: &[Filter],
    backend: Backend,
) -> (String, Vec<sea_query::Value>) {
    let mut query = Query::select();
    query
        .expr_as(Func::count(Expr::col(Asterisk)), Alias::new("cnt"))
        .from(DynCol(table.into()));

    if let Some(cond) = build_condition(filters) {
        query.cond_where(cond);
    }

    crate::render_select(query, backend)
}

/// Build SELECT COALESCE(SUM({field}), 0) FROM {table} WHERE {filters}.
pub fn build_sum(
    table: &str,
    field: &str,
    filters: &[Filter],
    backend: Backend,
) -> (String, Vec<sea_query::Value>) {
    let mut query = Query::select();
    query
        .expr_as(
            Func::coalesce([
                Func::sum(Expr::col(DynCol(field.into()))).into(),
                Expr::val(0i64).into(),
            ]),
            Alias::new("total"),
        )
        .from(DynCol(table.into()));

    if let Some(cond) = build_condition(filters) {
        query.cond_where(cond);
    }

    crate::render_select(query, backend)
}

/// Build a per-day count over a date window.
///
/// Produces (SQLite):
/// ```sql
/// SELECT date("created_at") AS day, COUNT(*) AS cnt
/// FROM {table}
/// WHERE {filters}
/// GROUP BY date("created_at")
/// ORDER BY day ASC
/// ```
///
/// Use this for charts that bucket events by day. Filters typically
/// include the date-window predicate (`date_field >= start AND
/// date_field < end`); the helper does not synthesize the window itself
/// so callers can layer extra predicates (e.g. `status = 'ERROR'`)
/// without re-implementing it.
pub fn build_daily_count(
    table: &str,
    date_field: &str,
    filters: &[Filter],
    backend: Backend,
) -> (String, Vec<sea_query::Value>) {
    use sea_query::SimpleExpr;

    // The column reference is interpolated into the raw expression text via
    // ANSI double-quoting (works for both SQLite and Postgres). `date_field`
    // is always an internal constant from caller code — no user input — so
    // we don't need parameterization here. Defensive guard rejects anything
    // that isn't a plain identifier just in case.
    assert!(
        date_field
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "date_field must be a plain identifier; got {date_field:?}"
    );
    let date_expr_sql: String = match backend {
        Backend::Sqlite => format!("date(\"{date_field}\")"),
        Backend::Postgres => {
            format!("to_char(CAST(\"{date_field}\" AS DATE), 'YYYY-MM-DD')")
        }
    };
    let date_expr: SimpleExpr = Expr::cust(&date_expr_sql);

    let mut query = Query::select();
    query
        .expr_as(date_expr.clone(), Alias::new("day"))
        .expr_as(Func::count(Expr::col(Asterisk)), Alias::new("cnt"))
        .from(DynCol(table.into()));

    if let Some(cond) = build_condition(filters) {
        query.cond_where(cond);
    }

    query.add_group_by(vec![date_expr]);
    query.order_by(Alias::new("day"), sea_query::Order::Asc);

    crate::render_select(query, backend)
}

/// Build SELECT AVG({field}) FROM {table} WHERE {filters}.
pub fn build_avg(
    table: &str,
    field: &str,
    filters: &[Filter],
    backend: Backend,
) -> (String, Vec<sea_query::Value>) {
    let mut query = Query::select();
    query
        .expr_as(
            Func::avg(Expr::col(DynCol(field.into()))),
            Alias::new("avg_val"),
        )
        .from(DynCol(table.into()));

    if let Some(cond) = build_condition(filters) {
        query.cond_where(cond);
    }

    crate::render_select(query, backend)
}

/// Aggregate function type.
#[derive(Debug, Clone)]
pub enum AggFunc {
    Count,
    Sum,
    Avg,
    Max,
    Min,
    /// `COALESCE(col, default)` — not a true aggregate, but a null-replacement
    /// wrapper that callers can fold into the aggregate-builder pipeline so
    /// they don't have to post-process null values in Rust. The JSON literal
    /// is rendered as the second argument to `COALESCE`.
    Coalesce(serde_json::Value),
}

/// A single aggregate column in a grouped query.
#[derive(Debug, Clone)]
pub struct AggregateColumn {
    /// Aggregate function to apply.
    pub func: AggFunc,
    /// Field to aggregate. None means * (for COUNT(*)).
    ///
    /// Ignored when [`inner_expr`](Self::inner_expr) is set.
    pub field: Option<String>,
    /// Output alias for this column.
    pub alias: String,
    /// Optional CAST type (e.g. "INTEGER" for CAST(AVG(...) AS INTEGER)).
    pub cast_as: Option<String>,
    /// Pre-built sea-query expression used as the aggregate's inner
    /// argument. When set, takes precedence over [`field`](Self::field)
    /// and lets callers express patterns the field/Asterisk shape can't —
    /// most commonly `SUM(CASE WHEN ... THEN ... ELSE ... END)` for
    /// conditional counts.
    ///
    /// Default-constructed via [`AggregateColumn::case_when_sum`] for
    /// the common "count rows matching predicate" pattern; see that
    /// constructor for an example.
    pub inner_expr: Option<SimpleExpr>,
}

impl AggregateColumn {
    /// Convenience constructor for `SUM(CASE WHEN <predicate> THEN 1 ELSE 0 END) AS <alias>`,
    /// a portable way to "count rows matching a predicate" inside a
    /// grouped query (no FILTER-clause support needed).
    ///
    /// # Example
    ///
    /// ```ignore
    /// use sea_query::Expr;
    /// use wafer_sql_utils::{aggregate::AggregateColumn, ident::DynCol};
    ///
    /// // SUM(CASE WHEN status_code >= 400 THEN 1 ELSE 0 END) AS errors
    /// let errors = AggregateColumn::case_when_sum(
    ///     "errors",
    ///     Expr::col(DynCol("status_code".into())).gte(400),
    /// );
    /// ```
    pub fn case_when_sum(alias: impl Into<String>, when: SimpleExpr) -> Self {
        let case: SimpleExpr = sea_query::CaseStatement::new()
            .case(when, SimpleExpr::Value(1i64.into()))
            .finally(SimpleExpr::Value(0i64.into()))
            .into();
        Self {
            func: AggFunc::Sum,
            field: None,
            alias: alias.into(),
            cast_as: None,
            inner_expr: Some(case),
        }
    }
}

/// Configuration for a grouped aggregate query.
#[derive(Debug, Clone)]
pub struct GroupedQueryConfig {
    pub table: String,
    /// Plain columns to select (not aggregated).
    pub select_columns: Vec<String>,
    /// Aggregate expressions.
    pub aggregates: Vec<AggregateColumn>,
    pub filters: Vec<Filter>,
    pub group_by: Vec<String>,
    pub order_by: Vec<SortField>,
    pub limit: Option<i64>,
}

/// Build a flexible grouped aggregate query.
///
/// Produces queries like:
/// ```sql
/// SELECT method, path, COUNT(*) as cnt, CAST(AVG(duration_ms) AS INTEGER) as avg_ms
/// FROM request_logs WHERE ... GROUP BY method, path ORDER BY cnt DESC LIMIT 50
/// ```
pub fn build_grouped_query(
    cfg: GroupedQueryConfig,
    backend: Backend,
) -> (String, Vec<sea_query::Value>) {
    let mut query = Query::select();
    query.from(DynCol(cfg.table.clone()));

    // Plain columns
    for col in &cfg.select_columns {
        query.column(DynCol(col.clone()));
    }

    // Aggregate columns
    for agg in &cfg.aggregates {
        // `inner_expr`, when set, wins over the `field`/`Asterisk` shape so
        // callers can express SUM(CASE WHEN ...), COUNT(DISTINCT ...), etc.
        let inner: SimpleExpr = match (&agg.inner_expr, &agg.field) {
            (Some(expr), _) => expr.clone(),
            (None, Some(f)) => Expr::col(DynCol(f.clone())).into(),
            (None, None) => Expr::col(Asterisk).into(),
        };

        let agg_expr: SimpleExpr = match &agg.func {
            AggFunc::Count => Func::count(inner).into(),
            AggFunc::Sum => Func::sum(inner).into(),
            AggFunc::Avg => Func::avg(inner).into(),
            AggFunc::Max => Func::max(inner).into(),
            AggFunc::Min => Func::min(inner).into(),
            AggFunc::Coalesce(default) => Func::coalesce([
                inner,
                Expr::val(crate::value::json_to_sea_value(default)).into(),
            ])
            .into(),
        };

        let final_expr: sea_query::SimpleExpr = if let Some(ref cast_type) = agg.cast_as {
            Expr::expr(agg_expr).cast_as(Alias::new(cast_type))
        } else {
            agg_expr
        };

        query.expr_as(final_expr, Alias::new(&agg.alias));
    }

    // WHERE
    if let Some(cond) = build_condition(&cfg.filters) {
        query.cond_where(cond);
    }

    // GROUP BY
    for col in &cfg.group_by {
        query.group_by_col(DynCol(col.clone()));
    }

    // ORDER BY
    apply_order(&mut query, &cfg.order_by);

    // LIMIT
    if let Some(limit) = cfg.limit {
        if limit > 0 {
            query.limit(limit as u64);
        }
    }

    crate::render_select(query, backend)
}

#[cfg(test)]
mod tests {
    use wafer_core::interfaces::database::service::FilterOp;

    use super::*;
    use crate::Backend;

    #[test]
    fn test_build_count_sqlite() {
        let filters = vec![Filter {
            field: "status".into(),
            operator: FilterOp::Equal,
            value: serde_json::json!("active"),
        }];
        let (sql, values) = build_count("users", &filters, Backend::Sqlite);
        assert!(sql.contains("COUNT(*)"));
        assert!(sql.contains("WHERE"));
        assert_eq!(values.len(), 1);
    }

    #[test]
    fn test_build_daily_count_sqlite() {
        let filters = vec![Filter {
            field: "created_at".into(),
            operator: FilterOp::GreaterEqual,
            value: serde_json::json!("2026-04-01"),
        }];
        let (sql, vals) = build_daily_count("users", "created_at", &filters, Backend::Sqlite);
        eprintln!("SQL: {sql}");
        eprintln!("VALS: {vals:?}");
        assert!(sql.contains("date("));
        assert!(sql.contains("COUNT(*)"));
        assert!(sql.contains("GROUP BY"));
        assert!(sql.contains("ORDER BY"));
        assert_eq!(vals.len(), 1);
    }

    #[test]
    fn test_build_daily_count_postgres() {
        let (sql, _vals) = build_daily_count("users", "created_at", &[], Backend::Postgres);
        assert!(sql.contains("to_char"));
        assert!(sql.contains("CAST"));
        assert!(sql.contains("GROUP BY"));
    }

    #[test]
    fn test_build_grouped_query() {
        let cfg = GroupedQueryConfig {
            table: "request_logs".into(),
            select_columns: vec!["method".into(), "path".into()],
            aggregates: vec![
                AggregateColumn {
                    func: AggFunc::Count,
                    field: None,
                    alias: "cnt".into(),
                    cast_as: None,
                    inner_expr: None,
                },
                AggregateColumn {
                    func: AggFunc::Avg,
                    field: Some("duration_ms".into()),
                    alias: "avg_ms".into(),
                    cast_as: Some("INTEGER".into()),
                    inner_expr: None,
                },
            ],
            filters: vec![],
            group_by: vec!["method".into(), "path".into()],
            order_by: vec![SortField {
                field: "cnt".into(),
                desc: true,
            }],
            limit: Some(50),
        };
        let (sql, _values) = build_grouped_query(cfg, Backend::Sqlite);
        assert!(sql.contains("COUNT(*)"));
        assert!(sql.contains("GROUP BY"));
        assert!(sql.contains("ORDER BY"));
        assert!(sql.contains("LIMIT"));
    }

    #[test]
    fn agg_func_coalesce_renders_sql() {
        let cfg = GroupedQueryConfig {
            table: "items".into(),
            select_columns: vec![],
            aggregates: vec![AggregateColumn {
                func: AggFunc::Coalesce(serde_json::json!(0)),
                field: Some("price".into()),
                alias: "price_or_zero".into(),
                cast_as: None,
                inner_expr: None,
            }],
            filters: vec![],
            group_by: vec!["category".into()],
            order_by: vec![],
            limit: None,
        };
        let (sql, _params) = build_grouped_query(cfg, Backend::Sqlite);
        eprintln!("SQL: {sql}");
        assert!(
            sql.to_uppercase().contains("COALESCE"),
            "expected COALESCE in: {sql}"
        );
        assert!(sql.contains("price"), "missing price column in: {sql}");
        // Accept either parameterised (`?`) or inlined (`0`) literal.
        assert!(
            sql.contains('0') || sql.contains('?'),
            "expected literal or placeholder in: {sql}"
        );
    }

    #[test]
    fn grouped_query_supports_case_when_sum() {
        use sea_query::Expr;
        let cfg = GroupedQueryConfig {
            table: "request_logs".into(),
            select_columns: vec!["method".into(), "path".into()],
            aggregates: vec![
                AggregateColumn {
                    func: AggFunc::Count,
                    field: None,
                    alias: "cnt".into(),
                    cast_as: None,
                    inner_expr: None,
                },
                AggregateColumn::case_when_sum(
                    "errors",
                    Expr::col(DynCol("status_code".into())).gte(400),
                ),
            ],
            filters: vec![],
            group_by: vec!["method".into(), "path".into()],
            order_by: vec![SortField {
                field: "cnt".into(),
                desc: true,
            }],
            limit: Some(50),
        };
        let (sql, _values) = build_grouped_query(cfg, Backend::Sqlite);
        // Both plain aggregate and CASE-WHEN aggregate render.
        assert!(sql.contains("COUNT(*)"), "missing COUNT in: {sql}");
        assert!(
            sql.contains("SUM(") && sql.contains("CASE WHEN"),
            "missing conditional SUM in: {sql}"
        );
        assert!(sql.contains("\"errors\""), "missing errors alias in: {sql}");
        assert!(sql.contains("GROUP BY"));
    }
}
