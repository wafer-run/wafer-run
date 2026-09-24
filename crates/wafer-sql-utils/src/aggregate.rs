use sea_query::{Alias, Asterisk, Cond, Expr, Func, Query, SimpleExpr};
use wafer_block::db::{Filter, SortField};

use crate::{
    ident::{validate_ident, DynCol},
    query::{apply_order, build_condition},
    Backend, SqlBuildError,
};

/// Shared tail of the single-aggregate builders:
/// `SELECT {expr} AS {alias} FROM {table} WHERE {filters}`, plus an optional
/// extra `Cond` AND-ed with the flat `filters` clause (see
/// [`crate::query::build_select_with_condition`] for the SELECT-side
/// equivalent — used to fold a `FilterTree`-derived `Cond` into a COUNT so
/// `total_count` matches the filtered row set, not the whole table).
fn agg_select(
    table: &str,
    expr: SimpleExpr,
    alias: &str,
    filters: &[Filter],
    extra_condition: Option<Cond>,
    backend: Backend,
) -> crate::Statement {
    let mut query = Query::select();
    query
        .expr_as(expr, Alias::new(alias))
        .from(DynCol(table.into()));

    if let Some(cond) = build_condition(filters) {
        query.cond_where(cond);
    }
    if let Some(extra) = extra_condition {
        query.cond_where(extra);
    }

    let (sql, values) = crate::render_select(query, backend);
    crate::Statement::new(sql, values, table)
}

/// Build SELECT COUNT(*) FROM {table} WHERE {filters}.
pub fn build_count(table: &str, filters: &[Filter], backend: Backend) -> crate::Statement {
    build_count_with_condition(table, filters, None, backend)
}

/// Build SELECT COUNT(*) FROM {table} WHERE {filters} AND {extra_condition}.
///
/// Use this when `filters` is the flat AND-of-filters list and there is also
/// a `FilterTree`-derived `Cond` (e.g. an OR group) that must be folded in —
/// most commonly to compute `total_count` for a `LIST` whose rows are
/// selected via [`crate::query::build_select_columns`] /
/// [`crate::query::build_select_with_condition`] with the same
/// `extra_condition`, so the count matches the actual filtered result set.
pub fn build_count_with_condition(
    table: &str,
    filters: &[Filter],
    extra_condition: Option<Cond>,
    backend: Backend,
) -> crate::Statement {
    agg_select(
        table,
        Func::count(Expr::col(Asterisk)).into(),
        "cnt",
        filters,
        extra_condition,
        backend,
    )
}

/// Build `SELECT COALESCE(CAST(SUM({field}) AS DOUBLE PRECISION), 0.0) FROM
/// {table} WHERE {filters}`.
///
/// The `sum` op decodes its scalar result as `f64`, so the sum is cast to
/// `DOUBLE PRECISION` in the SQL itself. On Postgres `SUM(<int column>)` is
/// `INT8` and `SUM(<bigint column>)` is `NUMERIC`, neither of which the `f64`
/// decode accepts; the fallback parameter takes its type from the sum it
/// stands in for, so it cannot widen the result. On SQLite the cast turns an
/// integer sum into a real, which the decode accepts either way.
pub fn build_sum(
    table: &str,
    field: &str,
    filters: &[Filter],
    backend: Backend,
) -> crate::Statement {
    let expr = Func::coalesce([
        Expr::expr(Func::sum(Expr::col(DynCol(field.into()))))
            .cast_as(Alias::new(CastType::Double.as_sql())),
        Expr::val(0.0_f64).into(),
    ]);
    agg_select(table, expr.into(), "total", filters, None, backend)
}

/// Per-dialect date-bucket expression — `date("field")` on SQLite,
/// `to_char(CAST("field" AS DATE), 'YYYY-MM-DD')` on Postgres — as a
/// sea-query custom expression. The single source shared by
/// [`build_daily_count`] and the grouped-query date-bucket path
/// ([`GroupedQueryConfig::date_buckets`]).
///
/// `field` is interpolated into raw expression text (ANSI double-quoted, valid
/// for both dialects), so it MUST be a validated plain identifier — callers
/// validate upstream (`build_daily_count` via [`validate_ident`]; the
/// aggregate handler via `validate_ident` in its `to_aggregate_spec`). Passing
/// an unvalidated field would let it break out of the surrounding expression.
fn date_bucket_expr(field: &str, backend: Backend) -> SimpleExpr {
    let sql = match backend {
        Backend::Sqlite => format!("date(\"{field}\")"),
        Backend::Postgres => format!("to_char(CAST(\"{field}\" AS DATE), 'YYYY-MM-DD')"),
    };
    Expr::cust(&sql)
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
///
/// Returns [`SqlBuildError::InvalidIdentifier`] if `date_field` is not a plain
/// identifier (`[A-Za-z0-9_]`). It is interpolated into the raw `date(...)` /
/// `to_char(...)` expression text rather than parameter-bound, so this is a
/// fail-closed guard, not a passthrough: we reject rather than splice an
/// identifier that could break out of the expression.
pub fn build_daily_count(
    table: &str,
    date_field: &str,
    filters: &[Filter],
    backend: Backend,
) -> Result<crate::Statement, SqlBuildError> {
    // The column reference is interpolated into the raw expression text via
    // ANSI double-quoting (works for both SQLite and Postgres), so it cannot be
    // parameter-bound. Reject anything that isn't a plain identifier rather
    // than risk it escaping the surrounding expression.
    let date_field = validate_ident(date_field)?;
    let date_expr = date_bucket_expr(date_field, backend);

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

    let (sql, values) = crate::render_select(query, backend);
    Ok(crate::Statement::new(sql, values, table))
}

/// Build SELECT AVG({field}) FROM {table} WHERE {filters}.
pub fn build_avg(
    table: &str,
    field: &str,
    filters: &[Filter],
    backend: Backend,
) -> crate::Statement {
    agg_select(
        table,
        Func::avg(Expr::col(DynCol(field.into()))).into(),
        "avg_val",
        filters,
        None,
        backend,
    )
}

/// Aggregate function type.
#[derive(Debug, Clone)]
pub enum AggFunc {
    /// `COUNT(...)` — row count over the inner expression (`*` if no field).
    Count,
    /// `SUM(...)` — numeric sum of the inner expression. `NULL` when no row
    /// contributes a non-null value (an empty table, or an empty set of rows
    /// in an ungrouped query).
    Sum,
    /// `COALESCE(SUM(...), 0)` — [`Sum`](Self::Sum), but `0` where `SUM`
    /// would be `NULL`. The `0` is an inline literal, so the result has the
    /// sum's type on Postgres.
    SumOrZero,
    /// `AVG(...)` — arithmetic mean of the inner expression.
    Avg,
    /// `MAX(...)` — greatest value of the inner expression.
    Max,
    /// `MIN(...)` — smallest value of the inner expression.
    Min,
    /// `COALESCE(col, default)` — not a true aggregate, but a null-replacement
    /// wrapper that callers can fold into the aggregate-builder pipeline so
    /// they don't have to post-process null values in Rust. The JSON literal
    /// is rendered as the second argument to `COALESCE`.
    Coalesce(serde_json::Value),
}

/// The SQL types an aggregate output may be cast to.
///
/// A closed set rather than a type-name string: the cast type is spliced into
/// the statement text (`CAST(<aggregate> AS <type>)`, never a bound
/// parameter), so an open string would be an injection vector. Every member
/// renders the same spelling on SQLite and Postgres and yields the same
/// storage class on both:
///
/// - [`BigInt`](Self::BigInt) — a 64-bit integer. Postgres widens
///   `SUM(<bigint>)` to `NUMERIC` and `AVG(<integer>)` to `NUMERIC`, and
///   SQLite returns a `REAL` sum over text-stored numbers; casting pins the
///   result to an integer on both. A non-integral value is rounded on
///   Postgres and truncated on SQLite, so cast only aggregates whose value is
///   integral — a sum of integers. An average is not one, which is why the
///   database handler accepts only `DOUBLE PRECISION` for `Avg`.
/// - [`Double`](Self::Double) — a double-precision float, for a result that
///   must read as a float on every backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastType {
    /// `BIGINT`.
    BigInt,
    /// `DOUBLE PRECISION`.
    Double,
}

impl CastType {
    /// Every allowed cast type, in the order they are documented.
    pub const ALL: [Self; 2] = [Self::BigInt, Self::Double];

    /// The SQL spelling spliced into `CAST(... AS <type>)`.
    #[must_use]
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::BigInt => "BIGINT",
            Self::Double => "DOUBLE PRECISION",
        }
    }

    /// Parse a caller-supplied type name against the allowlist. Matching is
    /// ASCII case-insensitive on the exact [`as_sql`](Self::as_sql) spelling;
    /// anything else is `None`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|t| t.as_sql().eq_ignore_ascii_case(name))
    }
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
    /// Optional output cast: `CAST(<aggregate> AS <type>)`.
    pub cast_as: Option<CastType>,
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
    /// Convenience constructor for
    /// `COALESCE(SUM(CASE WHEN <predicate> THEN 1 ELSE 0 END), 0) AS <alias>`,
    /// a portable way to "count rows matching a predicate" inside a
    /// grouped query (no FILTER-clause support needed). The count is `0`, not
    /// `NULL`, when no row matches — including an ungrouped query over no
    /// rows at all, where `SUM` alone is `NULL`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use sea_query::Expr;
    /// use wafer_sql_utils::{aggregate::AggregateColumn, ident::DynCol};
    ///
    /// // COALESCE(SUM(CASE WHEN status_code >= 400 THEN 1 ELSE 0 END), 0) AS errors
    /// let errors = AggregateColumn::case_when_sum(
    ///     "errors",
    ///     Expr::col(DynCol("status_code".into())).gte(400),
    /// );
    /// ```
    pub fn case_when_sum(alias: impl Into<String>, when: SimpleExpr) -> Self {
        // The THEN/ELSE operands are emitted as INLINE integer literals (`1` /
        // `0`), not bound parameters. A `CASE` whose branches are all bound
        // parameters gives Postgres nothing to infer their type from, so it
        // types them `text` and `SUM` refuses them; an inline `1`/`0` is an
        // `INT4` literal, so the SUM is `INT8`, which decodes cleanly as the
        // integer this conditional row-count is. SQLite is unaffected (it sums
        // to an integer either way).
        let case: SimpleExpr = sea_query::CaseStatement::new()
            .case(when, Expr::cust("1"))
            .finally(Expr::cust("0"))
            .into();
        Self {
            func: AggFunc::SumOrZero,
            field: None,
            alias: alias.into(),
            cast_as: None,
            inner_expr: Some(case),
        }
    }

    /// Convenience constructor for
    /// `COALESCE(SUM(CASE WHEN <predicate> THEN <field> ELSE 0 END), 0) AS <alias>`
    /// — the sum of `field` over the rows matching a predicate, inside a
    /// grouped query (no `FILTER` clause needed).
    ///
    /// The result is `0`, not `NULL`, whenever nothing non-null is summed: a
    /// group with no matching row (the `ELSE 0`), matching rows whose `field`
    /// is `NULL`, and an ungrouped query over no rows (the `COALESCE`). Both
    /// zeros are inline literals, as in [`case_when_sum`], so the `CASE` keeps
    /// `field`'s type (an inline `INT4` literal widens to it).
    /// `field` reaches sea-query as a quoted [`DynCol`]. The result is
    /// `SUM(<field type>)`, which Postgres widens to `NUMERIC` for a `BIGINT`
    /// column, so a caller that needs an integer sets
    /// [`cast_as`](Self::cast_as).
    ///
    /// [`case_when_sum`]: Self::case_when_sum
    pub fn sum_where(alias: impl Into<String>, field: impl Into<String>, when: SimpleExpr) -> Self {
        let case: SimpleExpr = sea_query::CaseStatement::new()
            .case(when, Expr::col(DynCol(field.into())))
            .finally(Expr::cust("0"))
            .into();
        Self {
            func: AggFunc::SumOrZero,
            field: None,
            alias: alias.into(),
            cast_as: None,
            inner_expr: Some(case),
        }
    }
}

/// A `GROUP BY date(field)` bucket for a [`GroupedQueryConfig`].
///
/// Groups rows by the day portion of a timestamp column and selects the
/// bucketed value under `alias`. Shares its per-dialect date expression with
/// [`build_daily_count`] (see [`date_bucket_expr`]).
///
/// `field` and `alias` reach raw expression text (not parameter-bound), so
/// callers MUST supply validated identifiers — the aggregate handler validates
/// every `DateBucket.field` with `validate_ident` before constructing this.
#[derive(Debug, Clone)]
pub struct DateBucketGroup {
    /// Timestamp column to bucket by day.
    pub field: String,
    /// Output alias for the bucketed date value.
    pub alias: String,
}

/// Configuration for a grouped aggregate query.
#[derive(Debug, Clone)]
pub struct GroupedQueryConfig {
    /// Source table name (interpolated as a quoted identifier).
    pub table: String,
    /// Plain columns to select (not aggregated).
    pub select_columns: Vec<String>,
    /// Aggregate expressions.
    pub aggregates: Vec<AggregateColumn>,
    /// `WHERE` predicates AND-ed together; rendered via
    /// [`crate::query::build_condition`].
    pub filters: Vec<Filter>,
    /// Columns to `GROUP BY` (interpolated as quoted identifiers).
    pub group_by: Vec<String>,
    /// Date-bucket `GROUP BY date(field)` terms, layered after the plain
    /// [`group_by`](Self::group_by) columns. Each also selects its bucketed
    /// value under its `alias`. Empty for non-time-series aggregates.
    pub date_buckets: Vec<DateBucketGroup>,
    /// `ORDER BY` clauses; alias names (e.g. `cnt`) are valid because the
    /// aggregates are emitted with `AS` aliases.
    pub order_by: Vec<SortField>,
    /// Optional `LIMIT N`; values `<= 0` are dropped.
    pub limit: Option<i64>,
}

/// Build a flexible grouped aggregate query.
///
/// Produces queries like:
/// ```sql
/// SELECT method, path, COUNT(*) as cnt, AVG(duration_ms) as avg_ms
/// FROM request_logs WHERE ... GROUP BY method, path ORDER BY cnt DESC LIMIT 50
/// ```
///
/// Takes `cfg` **by value** on purpose: [`GroupedQueryConfig`] is `!Send`
/// (its [`AggregateColumn`] expressions hold `Rc<dyn sea_query::Iden>`), so an
/// async caller that built the config inline must be able to *move and drop* it
/// before the next `.await`. A by-reference signature would keep the config
/// alive across the await point and make the caller's future `!Send`. The
/// `needless_pass_by_value` lint can't see that ownership transfer is the point.
#[expect(
    clippy::needless_pass_by_value,
    reason = "by-value lets async callers drop the !Send GroupedQueryConfig (Rc<dyn Iden>) before awaiting; a &ref signature would poison their futures' Send-ness"
)]
pub fn build_grouped_query(cfg: GroupedQueryConfig, backend: Backend) -> crate::Statement {
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
            AggFunc::SumOrZero => Func::coalesce([Func::sum(inner).into(), Expr::cust("0")]).into(),
            AggFunc::Avg => Func::avg(inner).into(),
            AggFunc::Max => Func::max(inner).into(),
            AggFunc::Min => Func::min(inner).into(),
            AggFunc::Coalesce(default) => Func::coalesce([
                inner,
                Expr::val(crate::value::json_to_sea_value(default)).into(),
            ])
            .into(),
        };

        let final_expr: sea_query::SimpleExpr = if let Some(cast_type) = agg.cast_as {
            Expr::expr(agg_expr).cast_as(Alias::new(cast_type.as_sql()))
        } else {
            agg_expr
        };

        query.expr_as(final_expr, Alias::new(&agg.alias));
    }

    // WHERE
    if let Some(cond) = build_condition(&cfg.filters) {
        query.cond_where(cond);
    }

    // GROUP BY — plain columns
    for col in &cfg.group_by {
        query.group_by_col(DynCol(col.clone()));
    }

    // GROUP BY — date buckets: select the bucketed value under its alias and
    // group by the same `date(field)` expression (fields validated upstream).
    for bucket in &cfg.date_buckets {
        let expr = date_bucket_expr(&bucket.field, backend);
        query.expr_as(expr.clone(), Alias::new(&bucket.alias));
        query.add_group_by(vec![expr]);
    }

    // ORDER BY
    apply_order(&mut query, &cfg.order_by);

    // LIMIT
    if let Some(limit) = cfg.limit {
        if limit > 0 {
            query.limit(limit as u64);
        }
    }

    let table = cfg.table.clone();
    let (sql, values) = crate::render_select(query, backend);
    crate::Statement::new(sql, values, table)
}

#[cfg(test)]
mod tests {
    use wafer_block::db::FilterOp;

    use super::*;
    use crate::Backend;

    #[test]
    fn test_build_count_sqlite() {
        let filters = vec![Filter {
            field: "status".into(),
            operator: FilterOp::Equal,
            value: serde_json::json!("active"),
        }];
        let stmt = build_count("users", &filters, Backend::Sqlite);
        let sql = stmt.sql;
        let values = stmt.values;
        assert!(sql.contains("COUNT(*)"));
        assert!(sql.contains("WHERE"));
        assert_eq!(values.len(), 1);
        assert_eq!(stmt.collection, "users");
    }

    #[test]
    fn test_build_daily_count_sqlite() {
        let filters = vec![Filter {
            field: "created_at".into(),
            operator: FilterOp::GreaterEqual,
            value: serde_json::json!("2026-04-01"),
        }];
        let stmt = build_daily_count("users", "created_at", &filters, Backend::Sqlite).unwrap();
        let sql = stmt.sql;
        let vals = stmt.values;
        eprintln!("SQL: {sql}");
        eprintln!("VALS: {vals:?}");
        assert!(sql.contains("date("));
        assert!(sql.contains("COUNT(*)"));
        assert!(sql.contains("GROUP BY"));
        assert!(sql.contains("ORDER BY"));
        assert_eq!(vals.len(), 1);
        assert_eq!(stmt.collection, "users");
    }

    #[test]
    fn test_build_daily_count_postgres() {
        let stmt = build_daily_count("users", "created_at", &[], Backend::Postgres).unwrap();
        let sql = stmt.sql;
        assert!(sql.contains("to_char"));
        assert!(sql.contains("CAST"));
        assert!(sql.contains("GROUP BY"));
        assert_eq!(stmt.collection, "users");
    }

    #[test]
    fn build_daily_count_rejects_non_identifier_date_field() {
        // `date_field` is interpolated into raw expression text, so a value
        // carrying anything outside [A-Za-z0-9_] must be rejected rather than
        // spliced — otherwise it could break out of the `date(...)` expression.
        let err = build_daily_count("events", "created_at\") OR 1=1 --", &[], Backend::Sqlite)
            .expect_err("non-identifier date_field should be rejected");
        assert_eq!(
            err,
            SqlBuildError::InvalidIdentifier {
                value: "created_at\") OR 1=1 --".to_string()
            }
        );
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
                    cast_as: Some(CastType::BigInt),
                    inner_expr: None,
                },
            ],
            filters: vec![],
            group_by: vec!["method".into(), "path".into()],
            date_buckets: vec![],
            order_by: vec![SortField {
                field: "cnt".into(),
                desc: true,
            }],
            limit: Some(50),
        };
        let stmt = build_grouped_query(cfg, Backend::Sqlite);
        let sql = stmt.sql;
        assert!(sql.contains("COUNT(*)"));
        assert!(sql.contains("GROUP BY"));
        assert!(sql.contains("ORDER BY"));
        assert!(sql.contains("LIMIT"));
        assert_eq!(stmt.collection, "request_logs");
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
            date_buckets: vec![],
            order_by: vec![],
            limit: None,
        };
        let stmt = build_grouped_query(cfg, Backend::Sqlite);
        let sql = stmt.sql;
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
        assert_eq!(stmt.collection, "items");
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
            date_buckets: vec![],
            order_by: vec![SortField {
                field: "cnt".into(),
                desc: true,
            }],
            limit: Some(50),
        };
        let stmt = build_grouped_query(cfg, Backend::Sqlite);
        let sql = stmt.sql;
        // Both plain aggregate and CASE-WHEN aggregate render.
        assert!(sql.contains("COUNT(*)"), "missing COUNT in: {sql}");
        assert!(
            sql.contains("SUM(") && sql.contains("CASE WHEN"),
            "missing conditional SUM in: {sql}"
        );
        assert!(sql.contains("\"errors\""), "missing errors alias in: {sql}");
        assert!(sql.contains("GROUP BY"));
        assert_eq!(stmt.collection, "request_logs");
    }

    #[test]
    fn grouped_query_supports_date_bucket_group() {
        // A `date_buckets` entry must both SELECT the bucketed value under its
        // alias and add the `date(field)` expression to GROUP BY — sharing the
        // exact per-dialect expression `build_daily_count` uses.
        let sqlite = build_grouped_query(
            GroupedQueryConfig {
                table: "events".into(),
                select_columns: vec![],
                aggregates: vec![AggregateColumn {
                    func: AggFunc::Count,
                    field: None,
                    alias: "cnt".into(),
                    cast_as: None,
                    inner_expr: None,
                }],
                filters: vec![],
                group_by: vec![],
                date_buckets: vec![DateBucketGroup {
                    field: "created_at".into(),
                    alias: "created_at".into(),
                }],
                order_by: vec![],
                limit: None,
            },
            Backend::Sqlite,
        );
        assert!(
            sqlite.sql.contains("date("),
            "sqlite date bucket: {}",
            sqlite.sql
        );
        assert!(sqlite.sql.contains("GROUP BY"), "{}", sqlite.sql);
        assert!(
            sqlite.sql.contains("\"created_at\""),
            "bucket alias missing: {}",
            sqlite.sql
        );
        assert_eq!(sqlite.collection, "events");

        // Postgres renders the `to_char(CAST(... AS DATE), ...)` form.
        let pg = build_grouped_query(
            GroupedQueryConfig {
                table: "events".into(),
                select_columns: vec![],
                aggregates: vec![AggregateColumn {
                    func: AggFunc::Count,
                    field: None,
                    alias: "cnt".into(),
                    cast_as: None,
                    inner_expr: None,
                }],
                filters: vec![],
                group_by: vec![],
                date_buckets: vec![DateBucketGroup {
                    field: "created_at".into(),
                    alias: "created_at".into(),
                }],
                order_by: vec![],
                limit: None,
            },
            Backend::Postgres,
        );
        assert!(pg.sql.contains("to_char"), "pg date bucket: {}", pg.sql);
        assert!(pg.sql.contains("CAST"), "{}", pg.sql);
        assert!(pg.sql.contains("GROUP BY"), "{}", pg.sql);
    }

    fn single_aggregate(agg: AggregateColumn, backend: Backend) -> String {
        build_grouped_query(
            GroupedQueryConfig {
                table: "orders".into(),
                select_columns: vec![],
                aggregates: vec![agg],
                filters: vec![],
                group_by: vec![],
                date_buckets: vec![],
                order_by: vec![],
                limit: None,
            },
            backend,
        )
        .sql
    }

    #[test]
    fn cast_as_wraps_the_aggregate_on_both_dialects() {
        for backend in [Backend::Sqlite, Backend::Postgres] {
            for (cast, spelled) in [
                (CastType::BigInt, "BIGINT"),
                (CastType::Double, "DOUBLE PRECISION"),
            ] {
                let sql = single_aggregate(
                    AggregateColumn {
                        func: AggFunc::Sum,
                        field: Some("total_cents".into()),
                        alias: "gross".into(),
                        cast_as: Some(cast),
                        inner_expr: None,
                    },
                    backend,
                );
                assert!(
                    sql.contains(&format!("CAST(SUM(\"total_cents\") AS {spelled})")),
                    "{backend:?} {cast:?}: {sql}"
                );
            }
        }
    }

    #[test]
    fn cast_type_parse_accepts_only_the_allowlist() {
        assert_eq!(CastType::parse("BIGINT"), Some(CastType::BigInt));
        assert_eq!(CastType::parse("bigint"), Some(CastType::BigInt));
        assert_eq!(CastType::parse("double precision"), Some(CastType::Double));
        for rejected in [
            "",
            "INTEGER",
            "TEXT",
            "BIGINT ",
            " BIGINT",
            "BIGINT) AS x, (SELECT 1",
            "DOUBLE",
            "NUMERIC",
        ] {
            assert_eq!(CastType::parse(rejected), None, "{rejected:?}");
        }
        for t in CastType::ALL {
            assert_eq!(CastType::parse(t.as_sql()), Some(t));
        }
    }

    #[test]
    fn sum_where_sums_the_field_over_matching_rows() {
        use sea_query::Expr;
        for (backend, placeholder) in [(Backend::Sqlite, "?"), (Backend::Postgres, "$1")] {
            let mut agg = AggregateColumn::sum_where(
                "refunded",
                "refunded_total_cents",
                Expr::col(DynCol("status".into())).eq("paid"),
            );
            agg.cast_as = Some(CastType::BigInt);
            assert_eq!(
                single_aggregate(agg, backend),
                format!(
                    "SELECT CAST(COALESCE(SUM((CASE WHEN (\"status\" = {placeholder}) THEN \
                     \"refunded_total_cents\" ELSE 0 END)), 0) AS BIGINT) AS \"refunded\" FROM \"orders\""
                ),
                "{backend:?}"
            );
        }
    }

    /// Run on a real SQLite engine: the conditional sum and count are `0`, not
    /// `NULL`, over matching rows whose field is `NULL` and over no rows at
    /// all — the two cases `SUM` alone answers with `NULL`.
    #[test]
    fn conditional_sums_are_zero_where_sum_alone_is_null() {
        use sea_query::Expr;
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE orders (status TEXT, refunded_total_cents INTEGER);
             INSERT INTO orders VALUES ('paid', NULL), ('paid', NULL);",
        )
        .unwrap();
        let run = |status: &str| -> (Option<i64>, Option<i64>) {
            let summed = AggregateColumn::sum_where(
                "summed",
                "refunded_total_cents",
                Expr::col(DynCol("status".into())).eq("paid"),
            );
            let counted = AggregateColumn::case_when_sum(
                "counted",
                Expr::col(DynCol("status".into())).eq("paid"),
            );
            let stmt = build_grouped_query(
                GroupedQueryConfig {
                    table: "orders".into(),
                    select_columns: vec![],
                    aggregates: vec![summed, counted],
                    filters: vec![Filter {
                        field: "status".into(),
                        operator: FilterOp::Equal,
                        value: serde_json::json!(status),
                    }],
                    group_by: vec![],
                    date_buckets: vec![],
                    order_by: vec![],
                    limit: None,
                },
                Backend::Sqlite,
            );
            let params: Vec<String> = stmt
                .values
                .iter()
                .map(|v| match v {
                    sea_query::Value::String(Some(s)) => s.to_string(),
                    other => panic!("unexpected bound value {other:?}"),
                })
                .collect();
            conn.query_row(&stmt.sql, rusqlite::params_from_iter(params), |r| {
                Ok((r.get("summed")?, r.get("counted")?))
            })
            .unwrap()
        };
        // Both rows match; their field is NULL.
        assert_eq!(run("paid"), (Some(0), Some(2)));
        // No row passes the WHERE.
        assert_eq!(run("refunded"), (Some(0), Some(0)));
    }
}
