//! Runs the shared, backend-agnostic [`DatabaseService`] conformance suite
//! against the PostgreSQL backend.
//!
//! Unlike the SQLite suite (which uses an in-memory database and always runs),
//! this needs a **live** PostgreSQL server, so it is gated behind the
//! `WAFER_CONFORMANCE_POSTGRES_URL` environment variable. When the variable is
//! unset the test is a no-op that prints a skip notice — so a default
//! `cargo test` stays green without a database. CI's `PostgreSQL Conformance`
//! job runs it against a `postgres:16` service container through
//! `scripts/check.sh postgres`, which fails rather than skips when the
//! variable is unset.
//!
//! To run it locally against a throwaway database:
//!
//! ```sh
//! docker run --rm -d -p 5432:5432 -e POSTGRES_PASSWORD=pw --name pg-conf postgres:16
//! WAFER_CONFORMANCE_POSTGRES_URL=postgres://postgres:pw@localhost:5432/postgres \
//!   cargo test -p wafer-block-postgres --test conformance -- --nocapture
//! ```
//!
//! The suite creates and drops its own `conf_*` tables, so it is safe to point
//! at any scratch database.
//!
//! This passes against a live server. The first live-DB run of the Postgres
//! backend surfaced four real, pre-existing backend bugs; three are now fixed at
//! the shared renderer / decoder layer and the suite exercises each (`sum` over
//! an `INT` column; windowed-counter upsert emitting an ambiguous column
//! reference; aggregate `CaseWhenSum` silently decoding to NULL). The fourth
//! (stamped RFC3339 string vs a real `TIMESTAMPTZ` column) is deferred — it
//! does not manifest for any real code, since every block stores timestamps in
//! TEXT columns. See the `conformance` module's "Backend divergences" section.
//! The test is gated off by default (no `WAFER_CONFORMANCE_POSTGRES_URL` →
//! skip), so `cargo test --workspace` stays green without a database.
//!
//! [`DatabaseService`]: wafer_core::interfaces::database::service::DatabaseService

use wafer_block_postgres::service::PostgresDatabaseService;
use wafer_core::interfaces::database::conformance::run_conformance;

const URL_ENV: &str = "WAFER_CONFORMANCE_POSTGRES_URL";

/// The PostgreSQL `DatabaseService` implementation must satisfy every op in
/// the shared conformance suite. Skipped unless `WAFER_CONFORMANCE_POSTGRES_URL`
/// points at a live server.
#[tokio::test]
async fn postgres_database_service_is_conformant() {
    let Ok(url) = std::env::var(URL_ENV) else {
        eprintln!("skipping postgres conformance: set {URL_ENV} to a live server URL to run");
        return;
    };

    let svc = PostgresDatabaseService::connect(&url)
        .await
        .expect("connect to the conformance PostgreSQL server");
    run_conformance(&svc).await;
}

/// A role granted only `SELECT` still gets the primary-key tiebreak.
///
/// `information_schema.table_constraints` hides a table's constraints from a
/// role that neither owns the table nor holds a privilege beyond `SELECT` on
/// it, so key introspection through the information schema came back empty
/// for such a role and its lists silently lost the tiebreak. The key is read
/// from `pg_catalog`, which every role can read. Skipped unless
/// `WAFER_CONFORMANCE_POSTGRES_URL` points at a live server whose user may
/// create roles.
#[tokio::test]
async fn a_select_only_role_still_breaks_ties_on_the_primary_key() {
    use std::str::FromStr as _;

    use sqlx::postgres::{PgConnectOptions, PgPool};
    use wafer_block::db::{ListOptions, SortField};
    use wafer_core::interfaces::database::service::DatabaseService;

    let Ok(url) = std::env::var(URL_ENV) else {
        eprintln!("skipping postgres read-only role check: set {URL_ENV} to run");
        return;
    };
    let admin = PgPool::connect(&url).await.expect("connect as admin");
    for stmt in [
        "DROP TABLE IF EXISTS conf_ro_tie",
        "DROP ROLE IF EXISTS conf_ro_reader",
        "CREATE TABLE conf_ro_tie (id TEXT PRIMARY KEY, created_at TEXT)",
        "INSERT INTO conf_ro_tie (id, created_at) VALUES \
         ('c', 't'), ('a', 't'), ('e', 't'), ('b', 't'), ('d', 't')",
        "CREATE ROLE conf_ro_reader LOGIN PASSWORD 'conf_ro_reader'",
        "GRANT SELECT ON conf_ro_tie TO conf_ro_reader",
    ] {
        sqlx::query(stmt)
            .execute(&admin)
            .await
            .unwrap_or_else(|e| panic!("{stmt}: {e}"));
    }

    let reader_opts = PgConnectOptions::from_str(&url)
        .expect("parse the conformance URL")
        .username("conf_ro_reader")
        .password("conf_ro_reader");
    let reader_pool = PgPool::connect_with(reader_opts)
        .await
        .expect("connect as the read-only role");
    let reader = PostgresDatabaseService::from_pool(reader_pool.clone());

    let mut ids = Vec::new();
    for offset in [0, 2, 4] {
        let page = reader
            .list(
                "conf_ro_tie",
                &ListOptions {
                    sort: vec![SortField {
                        field: "created_at".into(),
                        desc: true,
                    }],
                    limit: 2,
                    offset,
                    ..Default::default()
                },
            )
            .await
            .expect("list as the read-only role");
        ids.extend(page.records.into_iter().map(|r| r.id));
    }
    drop(reader);
    reader_pool.close().await;

    for stmt in ["DROP TABLE conf_ro_tie", "DROP ROLE conf_ro_reader"] {
        sqlx::query(stmt)
            .execute(&admin)
            .await
            .unwrap_or_else(|e| panic!("{stmt}: {e}"));
    }
    assert_eq!(
        ids,
        ["e", "d", "c", "b", "a"],
        "ties break id-descending for a role that can only read the table"
    );
}

/// A `PRIMARY KEY (...) INCLUDE (...)` index lists its covering columns in
/// `indkey` after its `indnkeyatts` key columns. They do not identify a row,
/// so the key introspection must stop at the key. (A list cannot show the
/// difference, since the key columns are already unique, so this reads the
/// introspection itself.) Skipped unless `WAFER_CONFORMANCE_POSTGRES_URL` is
/// set.
#[tokio::test]
async fn an_included_column_is_not_part_of_the_primary_key() {
    use sqlx::postgres::PgPool;
    use wafer_sql_utils::{introspect::build_list_primary_key, Backend};

    let Ok(url) = std::env::var(URL_ENV) else {
        eprintln!("skipping postgres INCLUDE key check: set {URL_ENV} to run");
        return;
    };
    let admin = PgPool::connect(&url).await.expect("connect");
    for stmt in [
        "DROP TABLE IF EXISTS conf_include_key",
        "CREATE TABLE conf_include_key (c TEXT, b TEXT, a TEXT, \
         PRIMARY KEY (a, b) INCLUDE (c))",
    ] {
        sqlx::query(stmt)
            .execute(&admin)
            .await
            .unwrap_or_else(|e| panic!("{stmt}: {e}"));
    }
    let (sql, params) = build_list_primary_key("conf_include_key", Backend::Postgres);
    let key: Vec<String> = sqlx::query_scalar(&sql)
        .bind(params[0].as_str().expect("bound table name"))
        .fetch_all(&admin)
        .await
        .expect("introspect the key");
    sqlx::query("DROP TABLE conf_include_key")
        .execute(&admin)
        .await
        .expect("drop");
    assert_eq!(key, ["a", "b"], "the INCLUDE column c is not a key column");
}

/// Concurrent guarded writes cannot overshoot their cap on PostgreSQL.
///
/// Under READ COMMITTED a guarded statement counts only committed rows, so
/// two statements that run side by side each miss the other's row. The
/// shared conformance suite races ten inserts, but nothing makes them
/// overlap; here a trigger holds every writing transaction open for 200 ms
/// after its row is written, so every racer's guard runs while the first
/// admitted write is still uncommitted. Only the per-table guard lock, which
/// makes each racer wait for the one before it to commit, keeps the count at
/// the cap. Skipped unless `WAFER_CONFORMANCE_POSTGRES_URL` is set.
#[tokio::test]
async fn racing_guarded_writes_cannot_overshoot_the_cap() {
    let Ok(url) = std::env::var(URL_ENV) else {
        eprintln!("skipping postgres guarded-write race: set {URL_ENV} to run");
        return;
    };
    race_guarded_writes(&url, "conf_guarded_race", None).await;
}

/// The same race on a server whose sessions default to REPEATABLE READ.
///
/// A REPEATABLE READ transaction takes its one snapshot at its first
/// statement — the lock statement, BEFORE the lock is granted — so a guard
/// run after waiting would still miss the writes it waited for. The guarded
/// transaction sets READ COMMITTED itself, so the session default cannot
/// reopen the race. Skipped unless `WAFER_CONFORMANCE_POSTGRES_URL` is set.
#[tokio::test]
async fn a_repeatable_read_session_default_cannot_reopen_the_race() {
    let Ok(url) = std::env::var(URL_ENV) else {
        eprintln!("skipping postgres guarded-write race (repeatable read): set {URL_ENV} to run");
        return;
    };
    race_guarded_writes(&url, "conf_guarded_race_rr", Some("repeatable read")).await;
}

/// Race eight guarded inserts under a cap of three files and five guarded
/// updates under a 60-byte cap on `table`, with every writing transaction
/// held open by a trigger, the service's sessions defaulting to
/// `isolation` when given; exactly the caps must land.
async fn race_guarded_writes(url: &str, table: &str, isolation: Option<&str>) {
    use std::collections::HashMap;

    use sqlx::postgres::{PgPool, PgPoolOptions};
    use wafer_block::db::{Filter, FilterOp};
    use wafer_core::interfaces::database::service::{
        pk, CapGuard, Column, DataType, DatabaseService, GuardedInsert, GuardedUpdate, Table,
    };

    let admin = PgPool::connect(url).await.expect("connect as admin");
    let session_default =
        isolation.map(|level| format!("SET default_transaction_isolation = '{level}'"));
    let pool = PgPoolOptions::new()
        .after_connect(move |conn, _| {
            let session_default = session_default.clone();
            Box::pin(async move {
                if let Some(stmt) = session_default {
                    sqlx::Executor::execute(conn, stmt.as_str()).await?;
                }
                Ok(())
            })
        })
        .connect(url)
        .await
        .expect("connect the service");
    if let Some(level) = isolation {
        let current: String = sqlx::query_scalar("SHOW transaction_isolation")
            .fetch_one(&pool)
            .await
            .expect("read the session isolation");
        assert_eq!(current, level, "the session default took effect");
    }
    let svc = PostgresDatabaseService::from_pool(pool);

    svc.schema_drop_table(table).await.expect("drop");
    svc.ensure_schema_table(&Table {
        name: table.into(),
        columns: vec![
            pk("id"),
            Column::new("owner", DataType::Text).null(),
            Column::new("size", DataType::Int64).null(),
            Column::new("created_at", DataType::Text).null(),
            Column::new("updated_at", DataType::Text).null(),
        ],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    })
    .await
    .expect("create");
    for stmt in [
        format!(
            "CREATE OR REPLACE FUNCTION {table}_linger() RETURNS trigger \
             LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(0.2); RETURN NEW; END $$"
        ),
        format!(
            "CREATE TRIGGER {table}_linger AFTER INSERT OR UPDATE ON {table} \
             FOR EACH ROW EXECUTE FUNCTION {table}_linger()"
        ),
    ] {
        sqlx::query(&stmt)
            .execute(&admin)
            .await
            .unwrap_or_else(|e| panic!("{stmt}: {e}"));
    }
    let owner = |o: &str| Filter {
        field: "owner".into(),
        operator: FilterOp::Equal,
        value: serde_json::json!(o),
    };
    let data = |pairs: &[(&str, serde_json::Value)]| -> HashMap<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    };

    // Eight inserts under a cap of three files.
    let cap = [CapGuard::CountBelow {
        filters: vec![owner("u")],
        cap: 3,
    }];
    let inserts = (0..8).map(|_| {
        svc.insert_guarded(
            table,
            data(&[
                ("owner", serde_json::json!("u")),
                ("size", serde_json::json!(1)),
            ]),
            &cap,
        )
    });
    let inserts = futures::future::join_all(inserts).await;
    let admitted = inserts
        .iter()
        .filter(|r| matches!(r, Ok(GuardedInsert::Inserted(_))))
        .count();
    let refused = inserts
        .iter()
        .filter(|r| matches!(r, Ok(GuardedInsert::Refused { guard: 0 })))
        .count();
    let files = svc.count(table, &[owner("u")]).await.expect("count");

    // Five 10-byte rows (50 of a 60-byte cap); five updates each grow a
    // different row to 20 bytes, its own old size excluded from the sum. Only
    // one fits: 40 + 20 = 60.
    for i in 0..5 {
        svc.create(
            table,
            data(&[
                ("id", serde_json::json!(format!("v{i}"))),
                ("owner", serde_json::json!("v")),
                ("size", serde_json::json!(10)),
            ]),
        )
        .await
        .expect("seed");
    }
    let guards: Vec<[CapGuard; 1]> = (0..5)
        .map(|i| {
            [CapGuard::SumAtMost {
                field: "size".into(),
                filters: vec![
                    owner("v"),
                    Filter {
                        field: "id".into(),
                        operator: FilterOp::NotEqual,
                        value: serde_json::json!(format!("v{i}")),
                    },
                ],
                add: 20,
                cap: 60,
            }]
        })
        .collect();
    let filters: Vec<[Filter; 1]> = (0..5)
        .map(|i| {
            [Filter {
                field: "id".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!(format!("v{i}")),
            }]
        })
        .collect();
    let updates = (0..5).map(|i| {
        svc.update_guarded(
            table,
            &filters[i],
            data(&[("size", serde_json::json!(20))]),
            &guards[i],
        )
    });
    let updates = futures::future::join_all(updates).await;
    let grown = updates
        .iter()
        .filter(|r| matches!(r, Ok(GuardedUpdate::Updated { rows_affected: 1 })))
        .count();
    let refused_updates = updates
        .iter()
        .filter(|r| matches!(r, Ok(GuardedUpdate::Refused { guard: 0 })))
        .count();
    let bytes = svc.sum(table, "size", &[owner("v")]).await.expect("sum");

    svc.schema_drop_table(table).await.expect("drop");
    sqlx::query(&format!("DROP FUNCTION {table}_linger()"))
        .execute(&admin)
        .await
        .expect("drop function");

    assert_eq!(
        (admitted, refused, files),
        (3, 5, 3),
        "eight racing inserts under a cap of three: {inserts:?}"
    );
    assert_eq!(
        (grown, refused_updates, bytes),
        (1, 4, 60.0),
        "five racing updates under a 60-byte cap: {updates:?}"
    );
}
