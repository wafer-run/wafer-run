use sea_query::{Asterisk, Cond, Expr, ExprTrait, Func, Query, SimpleExpr};
use wafer_block::db::Filter;

use crate::{
    ident::{validate_ident, DynCol},
    query::build_condition,
    value::json_to_sea_value,
    Backend, SqlBuildError,
};

/// A cap a guarded write must stay within, measured over the rows of the
/// written table that match `filters` as they stand BEFORE the write.
///
/// The guard does not see the row being written: the caller states what the
/// write adds (`CountBelow` counts one row; `SumAtMost` carries `add`) and,
/// for an update that replaces a row the aggregate already includes, excludes
/// that row with a filter (e.g. `id != <the row>`).
#[derive(Debug, Clone)]
pub enum CapGuard {
    /// Holds when fewer than `cap` rows match `filters` — so an insert that
    /// passes leaves at most `cap`.
    CountBelow {
        /// AND-combined predicates selecting the counted rows.
        filters: Vec<Filter>,
        /// Largest row count the write may leave.
        cap: i64,
    },
    /// Holds when `COALESCE(SUM(field), 0) + add <= cap` over the rows
    /// matching `filters`: landing exactly on `cap` passes.
    SumAtMost {
        /// Numeric column summed; a plain identifier.
        field: String,
        /// AND-combined predicates selecting the summed rows.
        filters: Vec<Filter>,
        /// What the write adds to the sum.
        add: i64,
        /// Largest total the write may leave.
        cap: i64,
    },
}

/// `(SELECT {aggregate} FROM {table} WHERE {filters})` as an expression.
fn aggregate_subquery(table: &str, aggregate: SimpleExpr, filters: &[Filter]) -> SimpleExpr {
    let mut sub = Query::select();
    sub.expr(aggregate).from(DynCol(table.into()));
    if let Some(cond) = build_condition(filters) {
        sub.cond_where(cond);
    }
    SimpleExpr::SubQuery(None, Box::new(sub.into_sub_query_statement()))
}

/// The predicate "`guard` holds".
///
/// The subqueries name `table` in their own `FROM`, so an unqualified column
/// in a guard's filters resolves to the counted rows, never to the row an
/// enclosing `UPDATE` is writing.
fn guard_holds(table: &str, guard: &CapGuard) -> Result<SimpleExpr, SqlBuildError> {
    Ok(match guard {
        CapGuard::CountBelow { filters, cap } => Expr::expr(aggregate_subquery(
            table,
            Func::count(Expr::col(Asterisk)).into(),
            filters,
        ))
        .lt(*cap),
        CapGuard::SumAtMost {
            field,
            filters,
            add,
            cap,
        } => {
            let field = validate_ident(field)?;
            let sum = Func::coalesce([
                Func::sum(Expr::col(DynCol(field.into()))).into(),
                Expr::val(0_i64).into(),
            ]);
            Expr::expr(aggregate_subquery(table, sum.into(), filters))
                .add(*add)
                .lte(*cap)
        }
    })
}

/// Every guard as one AND-combined predicate; `None` when there are none.
fn guard_condition(table: &str, guards: &[CapGuard]) -> Result<Option<Cond>, SqlBuildError> {
    if guards.is_empty() {
        return Ok(None);
    }
    let mut cond = Cond::all();
    for guard in guards {
        cond = cond.add(guard_holds(table, guard)?);
    }
    Ok(Some(cond))
}

/// Column of [`build_guard_probe`]'s row holding guard `index`'s verdict.
#[must_use]
pub fn guard_probe_column(index: usize) -> String {
    format!("g{index}")
}

/// Build `SELECT CASE WHEN {guard_0} THEN 1 ELSE 0 END AS g0, …`: one row
/// holding each guard's verdict (`1` holds, `0` refuses), named by
/// [`guard_probe_column`], evaluated exactly as the guarded write evaluates
/// it. Run it in the guarded write's transaction, just before the write, to
/// say which guard refused a write.
///
/// Returns [`SqlBuildError::InvalidIdentifier`] when a `SumAtMost` field is
/// not a plain identifier.
pub fn build_guard_probe(
    table: &str,
    guards: &[CapGuard],
    backend: Backend,
) -> Result<crate::Statement, SqlBuildError> {
    let mut select = Query::select();
    for (index, guard) in guards.iter().enumerate() {
        let verdict = sea_query::CaseStatement::new()
            .case(guard_holds(table, guard)?, Expr::val(1_i64))
            .finally(Expr::val(0_i64));
        select.expr_as(verdict, DynCol(guard_probe_column(index)));
    }
    let (sql, values) = crate::render_select(select, backend);
    Ok(crate::Statement::new(sql, values, table))
}

/// Build `INSERT INTO {table} (cols) SELECT vals WHERE {guards} RETURNING *`:
/// one statement that inserts the row only when every guard holds, returning
/// the stored row, or no row when a guard refused it.
///
/// One statement is atomic where writers are serialised (SQLite, D1). On
/// PostgreSQL under READ COMMITTED it is not — two concurrent statements each
/// count without the other's uncommitted row — so run it inside a transaction
/// that first takes [`build_guard_preamble`]'s lock. With no guards the row is
/// inserted unconditionally.
///
/// Returns [`SqlBuildError::InvalidIdentifier`] when a `SumAtMost` field is
/// not a plain identifier.
pub fn build_insert_guarded(
    table: &str,
    data: &[(String, serde_json::Value)],
    guards: &[CapGuard],
    backend: Backend,
) -> Result<crate::Statement, SqlBuildError> {
    let mut select = Query::select();
    for (_, value) in data {
        select.expr(SimpleExpr::from(json_to_sea_value(value)));
    }
    if let Some(cond) = guard_condition(table, guards)? {
        select.cond_where(cond);
    }

    let mut query = Query::insert();
    query
        .into_table(DynCol(table.into()))
        .columns(data.iter().map(|(k, _)| DynCol(k.clone())))
        .select_from(select)
        .expect("one selected value per inserted column, by construction");
    query.returning_all();
    let (sql, values) = crate::render_insert(query, backend);
    Ok(crate::Statement::new(sql, values, table))
}

/// Build `UPDATE {table} SET ... WHERE {filters} AND {guards}`: one statement
/// that updates the matching rows only when every guard holds; its affected
/// row count is 0 when a guard refused the write or no row matched (a
/// [`build_guard_probe`] in the same transaction tells the two apart).
///
/// The guards are evaluated once against the table before the update, not per
/// updated row. Atomicity is as for [`build_insert_guarded`]: run it after
/// [`build_guard_preamble`]'s lock on PostgreSQL.
///
/// Returns [`SqlBuildError::InvalidIdentifier`] when a `SumAtMost` field is
/// not a plain identifier.
pub fn build_update_guarded(
    table: &str,
    data: &[(String, serde_json::Value)],
    filters: &[Filter],
    guards: &[CapGuard],
    backend: Backend,
) -> Result<crate::Statement, SqlBuildError> {
    let mut query = Query::update();
    query.table(DynCol(table.into()));
    for (col, val) in data {
        query.value(DynCol(col.clone()), json_to_sea_value(val));
    }
    if let Some(cond) = build_condition(filters) {
        query.cond_where(cond);
    }
    if let Some(cond) = guard_condition(table, guards)? {
        query.cond_where(cond);
    }
    let (sql, values) = crate::render_update(query, backend);
    Ok(crate::Statement::new(sql, values, table))
}

/// Namespace of the guarded-write advisory locks, so they cannot collide
/// with an application's own single-key advisory locks.
const GUARD_LOCK_NAMESPACE: &str = "wafer.guarded_write";

/// The statements that serialise guarded writes to `table`, to run first in
/// the guarded write's transaction, in order; none where writers are already
/// serialised.
///
/// - PostgreSQL: `SET TRANSACTION ISOLATION LEVEL READ COMMITTED`, then
///   `SELECT pg_advisory_xact_lock(hashtext(ns), hashtext(table))`. The lock
///   is held to the end of the transaction, and under READ COMMITTED each
///   statement after it takes a fresh snapshot once the lock is granted, so
///   it counts every guarded write committed before it. The isolation level
///   is set explicitly because a server or role whose
///   `default_transaction_isolation` is REPEATABLE READ or SERIALIZABLE
///   would take the transaction's one snapshot at the lock statement —
///   BEFORE the lock is granted — and the guard would then miss the writes
///   it waited for.
/// - SQLite (and D1, which renders as SQLite): none. A write transaction
///   holds the database's single write lock, so no other write interleaves.
///
/// The key is the TABLE, not the guard's filters: guards with different
/// filters over the same rows (a count per `(owner, bucket)` and a byte sum
/// per `owner`, or a sum that excludes the row an update replaces) must still
/// exclude each other, and filter-derived keys would let them run side by
/// side. The cost is that guarded writes to one table run one at a time on
/// PostgreSQL, as every write already does on SQLite. Unguarded writes do not
/// take the lock, so a guard is exact only against other guarded writes.
#[must_use]
pub fn build_guard_preamble(table: &str, backend: Backend) -> Vec<crate::Statement> {
    match backend {
        Backend::Sqlite => Vec::new(),
        Backend::Postgres => vec![
            crate::Statement::new(
                "SET TRANSACTION ISOLATION LEVEL READ COMMITTED".to_string(),
                Vec::new(),
                table,
            ),
            crate::Statement::new(
                "SELECT pg_advisory_xact_lock(hashtext($1), hashtext($2))".to_string(),
                vec![
                    GUARD_LOCK_NAMESPACE.into(),
                    sea_query::Value::from(table.to_string()),
                ],
                table,
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use wafer_block::db::FilterOp;

    use super::*;
    use crate::value::sea_values_to_json;

    fn eq(field: &str, value: serde_json::Value) -> Filter {
        Filter {
            field: field.into(),
            operator: FilterOp::Equal,
            value,
        }
    }

    fn row(id: &str, owner: &str, size: i64) -> Vec<(String, serde_json::Value)> {
        vec![
            ("id".into(), serde_json::json!(id)),
            ("owner".into(), serde_json::json!(owner)),
            ("size".into(), serde_json::json!(size)),
        ]
    }

    fn db() -> Connection {
        let db = Connection::open_in_memory().expect("open");
        db.execute_batch(
            "CREATE TABLE files (id TEXT PRIMARY KEY, owner TEXT, size INTEGER, note TEXT)",
        )
        .expect("create");
        db
    }

    /// Run a rendered SQLite statement, returning the rows it returned (for an
    /// INSERT … RETURNING) or the affected count (for an UPDATE).
    fn run(db: &Connection, stmt: &crate::Statement) -> usize {
        let params: Vec<rusqlite::types::Value> = sea_values_to_json(stmt.values.clone())
            .into_iter()
            .map(|v| match v {
                serde_json::Value::Null => rusqlite::types::Value::Null,
                serde_json::Value::Number(n) => n.as_i64().map_or(
                    rusqlite::types::Value::Null,
                    rusqlite::types::Value::Integer,
                ),
                serde_json::Value::String(s) => rusqlite::types::Value::Text(s),
                other => rusqlite::types::Value::Text(other.to_string()),
            })
            .collect();
        let mut prepared = db.prepare(&stmt.sql).expect(&stmt.sql);
        if stmt.sql.contains("RETURNING") {
            let mut rows = prepared
                .query(rusqlite::params_from_iter(params))
                .expect("query");
            let mut n = 0;
            while rows.next().expect("row").is_some() {
                n += 1;
            }
            n
        } else {
            prepared
                .execute(rusqlite::params_from_iter(params))
                .expect("execute")
        }
    }

    fn count(db: &Connection) -> i64 {
        db.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .expect("count")
    }

    #[test]
    fn insert_renders_one_conditional_insert_select_on_both_backends() {
        let guards = [
            CapGuard::CountBelow {
                filters: vec![eq("owner", serde_json::json!("u"))],
                cap: 3,
            },
            CapGuard::SumAtMost {
                field: "size".into(),
                filters: vec![eq("owner", serde_json::json!("u"))],
                add: 5,
                cap: 100,
            },
        ];
        let lite = build_insert_guarded("files", &row("a", "u", 5), &guards, Backend::Sqlite)
            .expect("sqlite");
        assert!(
            lite.sql
                .starts_with(r#"INSERT INTO "files" ("id", "owner", "size") SELECT ?, ?, ? WHERE"#),
            "{}",
            lite.sql
        );
        assert!(
            lite.sql
                .contains(r#"(SELECT COUNT(*) FROM "files" WHERE "owner" = ?) < ?"#),
            "{}",
            lite.sql
        );
        assert!(
            lite.sql.contains(
                r#"(SELECT COALESCE(SUM("size"), ?) FROM "files" WHERE "owner" = ?) + ? <= ?"#
            ),
            "{}",
            lite.sql
        );
        assert!(lite.sql.ends_with("RETURNING *"), "{}", lite.sql);

        let pg = build_insert_guarded("files", &row("a", "u", 5), &guards, Backend::Postgres)
            .expect("postgres");
        assert!(pg.sql.contains("SELECT $1, $2, $3 WHERE"), "{}", pg.sql);
        assert_eq!(pg.values.len(), lite.values.len());
    }

    #[test]
    fn insert_lands_only_while_the_count_is_below_the_cap() {
        let db = db();
        let insert = |id: &str, owner: &str| {
            let guards = [CapGuard::CountBelow {
                filters: vec![eq("owner", serde_json::json!(owner))],
                cap: 2,
            }];
            let stmt = build_insert_guarded("files", &row(id, owner, 1), &guards, Backend::Sqlite)
                .expect("build");
            run(&db, &stmt)
        };
        let results: Vec<usize> = ["a", "b", "c"].iter().map(|id| insert(id, "u")).collect();
        assert_eq!(results, [1, 1, 0], "the third insert is refused");
        // `u`'s rows are not counted against another owner's cap.
        assert_eq!(insert("d", "v"), 1);
        assert_eq!(count(&db), 3);
    }

    #[test]
    fn sum_guard_admits_landing_exactly_on_the_cap_and_refuses_one_past_it() {
        let db = db();
        let guard = |add: i64| {
            [CapGuard::SumAtMost {
                field: "size".into(),
                filters: vec![eq("owner", serde_json::json!("u"))],
                add,
                cap: 10,
            }]
        };
        let insert = |id: &str, size: i64| {
            build_insert_guarded("files", &row(id, "u", size), &guard(size), Backend::Sqlite)
                .expect("build")
        };
        assert_eq!(run(&db, &insert("a", 6)), 1, "0 + 6 <= 10");
        assert_eq!(run(&db, &insert("b", 4)), 1, "6 + 4 == 10 lands");
        assert_eq!(run(&db, &insert("c", 1)), 0, "10 + 1 > 10 is refused");
        assert_eq!(count(&db), 2);
    }

    #[test]
    fn update_is_refused_when_a_guard_fails_and_counts_the_table_not_the_target() {
        let db = db();
        for (id, size) in [("a", 4), ("b", 4)] {
            let stmt = crate::query::build_insert("files", &row(id, "u", size), Backend::Sqlite);
            run(&db, &stmt);
        }
        // Grow `a` from 4 to 6: the other rows (b = 4) + 6 = 10 <= 10.
        let grow = |to: i64| {
            build_update_guarded(
                "files",
                &[("size".into(), serde_json::json!(to))],
                &[eq("id", serde_json::json!("a"))],
                &[CapGuard::SumAtMost {
                    field: "size".into(),
                    filters: vec![
                        eq("owner", serde_json::json!("u")),
                        Filter {
                            field: "id".into(),
                            operator: FilterOp::NotEqual,
                            value: serde_json::json!("a"),
                        },
                    ],
                    add: to,
                    cap: 10,
                }],
                Backend::Sqlite,
            )
            .expect("build")
        };
        assert_eq!(run(&db, &grow(6)), 1);
        assert_eq!(run(&db, &grow(7)), 0, "4 + 7 > 10");
        let size: i64 = db
            .query_row("SELECT size FROM files WHERE id = 'a'", [], |r| r.get(0))
            .expect("size");
        assert_eq!(size, 6, "the refused update changed nothing");
    }

    #[test]
    fn the_probe_reports_each_guards_verdict_as_the_write_evaluates_it() {
        let db = db();
        let insert = crate::query::build_insert("files", &row("a", "u", 8), Backend::Sqlite);
        run(&db, &insert);
        let guards = [
            CapGuard::CountBelow {
                filters: vec![eq("owner", serde_json::json!("u"))],
                cap: 5,
            },
            CapGuard::SumAtMost {
                field: "size".into(),
                filters: vec![eq("owner", serde_json::json!("u"))],
                add: 3,
                cap: 10,
            },
        ];
        let probe = build_guard_probe("files", &guards, Backend::Sqlite).expect("probe");
        let params: Vec<rusqlite::types::Value> = sea_values_to_json(probe.values)
            .into_iter()
            .map(|v| match v {
                serde_json::Value::Number(n) => {
                    rusqlite::types::Value::Integer(n.as_i64().expect("integer"))
                }
                serde_json::Value::String(s) => rusqlite::types::Value::Text(s),
                other => panic!("unexpected probe parameter {other}"),
            })
            .collect();
        let verdicts: (i64, i64) = db
            .query_row(&probe.sql, rusqlite::params_from_iter(params), |r| {
                Ok((
                    r.get(guard_probe_column(0).as_str())?,
                    r.get(guard_probe_column(1).as_str())?,
                ))
            })
            .expect(&probe.sql);
        assert_eq!(verdicts, (1, 0), "1 file < 5 holds; 8 + 3 > 10 refuses");
    }

    #[test]
    fn no_guards_is_an_unconditional_write() {
        let db = db();
        let stmt =
            build_insert_guarded("files", &row("a", "u", 1), &[], Backend::Sqlite).expect("build");
        assert!(!stmt.sql.contains("WHERE"), "{}", stmt.sql);
        assert_eq!(run(&db, &stmt), 1);
    }

    #[test]
    fn a_sum_field_that_is_not_a_plain_identifier_is_refused() {
        let guards = [CapGuard::SumAtMost {
            field: "size\"); DROP TABLE files; --".into(),
            filters: Vec::new(),
            add: 0,
            cap: 0,
        }];
        assert!(matches!(
            build_insert_guarded("files", &row("a", "u", 1), &guards, Backend::Sqlite),
            Err(SqlBuildError::InvalidIdentifier { .. })
        ));
        assert!(matches!(
            build_update_guarded("files", &[], &[], &guards, Backend::Postgres),
            Err(SqlBuildError::InvalidIdentifier { .. })
        ));
    }

    #[test]
    fn only_postgres_takes_a_guard_lock_and_it_is_keyed_by_table() {
        assert!(build_guard_preamble("files", Backend::Sqlite).is_empty());
        let mut preamble = build_guard_preamble("files", Backend::Postgres).into_iter();
        let isolation = preamble.next().expect("isolation statement");
        assert_eq!(
            isolation.sql,
            "SET TRANSACTION ISOLATION LEVEL READ COMMITTED"
        );
        let lock = preamble.next().expect("lock statement");
        assert!(lock.sql.contains("pg_advisory_xact_lock"), "{}", lock.sql);
        assert!(preamble.next().is_none());
        assert_eq!(
            sea_values_to_json(lock.values),
            [
                serde_json::json!(GUARD_LOCK_NAMESPACE),
                serde_json::json!("files")
            ]
        );
    }
}
