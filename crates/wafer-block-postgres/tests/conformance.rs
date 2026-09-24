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
//! backend surfaced four real, pre-existing backend bugs, all now fixed: the
//! suite exercises three (`sum` over an `INT` column; windowed-counter upsert
//! emitting an ambiguous column reference; aggregate `CaseWhenSum` silently
//! decoding to NULL), and
//! [`a_stamped_timestamp_binds_into_a_timestamptz_column`] the fourth (stamped
//! RFC3339 string vs a real `TIMESTAMPTZ` column). See the `conformance`
//! module's "Backend divergences" section.
//! The test is gated off by default (no `WAFER_CONFORMANCE_POSTGRES_URL` →
//! skip), so `cargo test --workspace` stays green without a database.
//!
//! [`DatabaseService`]: wafer_core::interfaces::database::service::DatabaseService

use wafer_block_postgres::service::PostgresDatabaseService;
use wafer_core::interfaces::database::conformance::{
    run_conformance, run_two_instance_conformance,
};

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

/// Two pools on one database — two replicas' worth of connections and schema
/// caches — must not hide each other's schema changes: a table one saw
/// missing and the other created is visible to the first. Skipped unless
/// `WAFER_CONFORMANCE_POSTGRES_URL` points at a live server.
#[tokio::test]
async fn two_postgres_services_on_one_database_see_each_others_tables() {
    let Ok(url) = std::env::var(URL_ENV) else {
        eprintln!("skipping postgres two-instance conformance: set {URL_ENV} to run");
        return;
    };
    let a = PostgresDatabaseService::connect(&url)
        .await
        .expect("connect the first service");
    let b = PostgresDatabaseService::connect(&url)
        .await
        .expect("connect the second service");
    run_two_instance_conformance(&a, &b).await;
}

/// A service whose sessions resolve names through a `search_path` without
/// `public` is conformant: the statements are unqualified, so they reach the
/// tables in the session's schema, and the introspection behind every
/// existence, column and key check must reach the same ones. Runs the whole
/// suite, and the two-instance suite, in a fresh schema. Skipped unless
/// `WAFER_CONFORMANCE_POSTGRES_URL` is set.
#[tokio::test]
async fn a_search_path_without_public_is_conformant() {
    use std::str::FromStr as _;

    use sqlx::postgres::{PgConnectOptions, PgPool};
    use wafer_sql_utils::{
        introspect::{build_list_tables, build_list_tables_like, build_table_info},
        Backend,
    };

    const SCHEMA: &str = "conf_search_path";
    let Ok(url) = std::env::var(URL_ENV) else {
        eprintln!("skipping postgres search_path conformance: set {URL_ENV} to run");
        return;
    };
    let admin = PgPool::connect(&url).await.expect("connect as admin");
    for stmt in [
        format!("DROP SCHEMA IF EXISTS {SCHEMA} CASCADE"),
        format!("CREATE SCHEMA {SCHEMA}"),
    ] {
        sqlx::query(&stmt)
            .execute(&admin)
            .await
            .unwrap_or_else(|e| panic!("{stmt}: {e}"));
    }
    let in_schema = || async {
        let options = PgConnectOptions::from_str(&url)
            .expect("parse the conformance URL")
            .options([("search_path", SCHEMA)]);
        PostgresDatabaseService::from_pool(
            PgPool::connect_with(options)
                .await
                .expect("connect with the search_path"),
        )
        .expect("service")
    };
    let svc = in_schema().await;
    run_conformance(&svc).await;
    run_two_instance_conformance(&svc, &in_schema().await).await;

    // The admin listings resolve names the same way.
    let options = PgConnectOptions::from_str(&url)
        .expect("parse the conformance URL")
        .options([("search_path", SCHEMA)]);
    let session = PgPool::connect_with(options).await.expect("connect");
    sqlx::query("CREATE TABLE conf_sp_probe (id TEXT PRIMARY KEY, n INTEGER NOT NULL)")
        .execute(&session)
        .await
        .expect("create the probe table");
    let tables: Vec<String> = sqlx::query_scalar(&build_list_tables(Backend::Postgres))
        .fetch_all(&session)
        .await
        .expect("list tables");
    assert!(tables.contains(&"conf_sp_probe".to_string()), "{tables:?}");
    let (sql, params) = build_list_tables_like("conf_sp_", Backend::Postgres);
    let like: Vec<String> = sqlx::query_scalar(&sql)
        .bind(params[0].as_str().expect("pattern"))
        .fetch_all(&session)
        .await
        .expect("list tables like");
    assert_eq!(like, ["conf_sp_probe"]);
    let (sql, params) = build_table_info("conf_sp_probe", Backend::Postgres).expect("valid name");
    let info: Vec<(String, String, String)> = sqlx::query_as(&format!(
        "SELECT column_name, data_type, is_nullable FROM ({sql}) AS info"
    ))
    .bind(params[0].as_str().expect("table name"))
    .fetch_all(&session)
    .await
    .expect("table info");
    assert_eq!(
        info,
        [
            ("id".into(), "text".into(), "NO".into()),
            ("n".into(), "integer".into(), "NO".into()),
        ]
    );
    session.close().await;

    sqlx::query(&format!("DROP SCHEMA {SCHEMA} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop the schema");
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
    let reader = PostgresDatabaseService::from_pool(reader_pool.clone()).expect("service");

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
                    limit: Some(2),
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
    let svc = PostgresDatabaseService::from_pool(pool).expect("service");

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

/// Two sessions creating the same table at once collide on a catalog index
/// (`pg_type_typname_nsp_index`, SQLSTATE 23505) even under `IF NOT EXISTS`.
/// That is a DDL race, not a duplicate row: it must stay `Internal`, never
/// `AlreadyExists`. An uncommitted `CREATE TABLE` in another session forces
/// the collision: the service's create waits on it, then fails once it
/// commits. Skipped unless `WAFER_CONFORMANCE_POSTGRES_URL` is set.
#[tokio::test]
async fn a_catalog_collision_is_not_already_exists() {
    use sqlx::postgres::PgPool;
    use wafer_core::interfaces::database::service::{DatabaseError, DatabaseService};

    let Ok(url) = std::env::var(URL_ENV) else {
        eprintln!("skipping postgres catalog-collision check: set {URL_ENV} to run");
        return;
    };
    let table = "conf_catalog_race";
    let admin = PgPool::connect(&url).await.expect("connect as admin");
    sqlx::query(&format!("DROP TABLE IF EXISTS {table}"))
        .execute(&admin)
        .await
        .expect("drop");
    let svc = PostgresDatabaseService::connect(&url)
        .await
        .expect("connect the service");

    let mut other = admin.begin().await.expect("begin the other session");
    sqlx::query(&format!("CREATE TABLE {table} (id TEXT PRIMARY KEY)"))
        .execute(&mut *other)
        .await
        .expect("create in the other session");
    // Through `exec_raw`, the path a migration runner takes: the driver's
    // error reaches the classifier unwrapped.
    let statement = format!("CREATE TABLE IF NOT EXISTS {table} (id TEXT PRIMARY KEY)");
    let create = svc.exec_raw(&statement, &[]);
    let commit = async {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        other.commit().await.expect("commit the other session");
    };
    let (created, ()) = tokio::join!(create, commit);

    sqlx::query(&format!("DROP TABLE IF EXISTS {table}"))
        .execute(&admin)
        .await
        .expect("drop");
    match created {
        Err(DatabaseError::Internal(msg)) => assert!(
            msg.contains("23505") || msg.contains("duplicate key"),
            "{msg}"
        ),
        other => panic!("a catalog collision must be Internal, got {other:?}"),
    }
}

/// The RFC3339 string `create` stamps into `created_at`/`updated_at`, and one
/// a caller writes or filters by, binds into a real `TIMESTAMPTZ` column.
///
/// Parameters bind by the type Postgres infers for them, so the same string
/// is a timestamp for a `TIMESTAMPTZ` column and text for a TEXT one. Bound
/// as text it was refused (`column … is of type timestamp with time zone but
/// expression is of type text`). The stored form reads back in Postgres's own
/// RFC3339 spelling, which is why this is not a shared conformance check.
/// Skipped unless `WAFER_CONFORMANCE_POSTGRES_URL` is set.
#[tokio::test]
async fn a_stamped_timestamp_binds_into_a_timestamptz_column() {
    use std::collections::HashMap;

    use wafer_block::db::{Filter, FilterOp, ListOptions};
    use wafer_core::interfaces::database::service::{
        pk, timestamps, Column, DataType, DatabaseService, Table,
    };

    let Ok(url) = std::env::var(URL_ENV) else {
        eprintln!("skipping postgres TIMESTAMPTZ check: set {URL_ENV} to run");
        return;
    };
    let svc = PostgresDatabaseService::connect(&url)
        .await
        .expect("connect to the conformance PostgreSQL server");
    let mut columns = vec![pk("id"), Column::new("due", DataType::DateTime).null()];
    columns.extend(timestamps());
    let table = Table {
        name: "conf_timestamptz".to_string(),
        columns,
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    };
    svc.schema_drop_table(&table.name).await.expect("drop");
    svc.ensure_schema_table(&table).await.expect("create");

    let row: HashMap<String, serde_json::Value> = [
        ("id".to_string(), serde_json::json!("t1")),
        ("due".to_string(), serde_json::json!("2026-01-15T10:00:00Z")),
    ]
    .into_iter()
    .collect();
    svc.create(&table.name, row)
        .await
        .expect("create stamps created_at/updated_at into TIMESTAMPTZ columns");

    let got = svc.get(&table.name, "t1").await.expect("get");
    let due = got.data["due"]
        .as_str()
        .expect("due reads back as a string");
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(due).expect("RFC3339"),
        chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z").expect("RFC3339"),
    );
    for stamped in ["created_at", "updated_at"] {
        let at = got.data[stamped].as_str().expect("stamped timestamp");
        chrono::DateTime::parse_from_rfc3339(at).expect("the stamp reads back as RFC3339");
    }

    let before = |at: &str| ListOptions {
        filters: vec![Filter {
            field: "due".to_string(),
            operator: FilterOp::LessThan,
            value: serde_json::json!(at),
        }],
        ..Default::default()
    };
    let listed = |at: &'static str| {
        let svc = &svc;
        let table = &table.name;
        async move {
            svc.list(table, &before(at))
                .await
                .expect("filter a TIMESTAMPTZ column by an RFC3339 string")
                .records
                .len()
        }
    };
    assert_eq!(listed("2026-02-01T00:00:00Z").await, 1);
    assert_eq!(listed("2026-01-01T00:00:00+01:00").await, 0);

    svc.schema_drop_table(&table.name).await.expect("drop");
}

/// A service connected with `search_path = first, second`, where both
/// schemas hold a table named `conf_dup` with different columns and a
/// second table exists only in `second`.
async fn two_schema_session(url: &str) -> (sqlx::PgPool, sqlx::PgPool, PostgresDatabaseService) {
    use std::str::FromStr as _;

    use sqlx::postgres::{PgConnectOptions, PgPool};

    let admin = PgPool::connect(url).await.expect("connect as admin");
    for stmt in [
        "DROP SCHEMA IF EXISTS conf_sp_first CASCADE",
        "DROP SCHEMA IF EXISTS conf_sp_second CASCADE",
        "CREATE SCHEMA conf_sp_first",
        "CREATE SCHEMA conf_sp_second",
        "CREATE TABLE conf_sp_first.conf_dup (id TEXT PRIMARY KEY, only_first TEXT)",
        "CREATE TABLE conf_sp_second.conf_dup (id TEXT PRIMARY KEY, only_second TEXT)",
        "INSERT INTO conf_sp_second.conf_dup (id, only_second) VALUES ('shadowed', 'x')",
        "CREATE TABLE conf_sp_second.conf_second_only (id TEXT PRIMARY KEY, name TEXT)",
    ] {
        sqlx::query(stmt)
            .execute(&admin)
            .await
            .unwrap_or_else(|e| panic!("{stmt}: {e}"));
    }
    // No space after the comma: the connect options split on whitespace.
    let options = PgConnectOptions::from_str(url)
        .expect("parse the conformance URL")
        .options([("search_path", "conf_sp_first,conf_sp_second")]);
    let session = PgPool::connect_with(options)
        .await
        .expect("connect with the search_path");
    let svc = PostgresDatabaseService::from_pool(session.clone()).expect("service");
    (admin, session, svc)
}

/// With the same table name in two schemas on the `search_path`, every
/// introspection answers for the table an unqualified statement reaches —
/// the one in the first schema — and a table only the second schema holds
/// is still found. Skipped unless `WAFER_CONFORMANCE_POSTGRES_URL` is set.
#[tokio::test]
async fn a_name_in_two_schemas_resolves_as_the_statements_do() {
    use wafer_block::db::{Filter, FilterOp, ListOptions};
    use wafer_core::interfaces::database::service::{DatabaseError, DatabaseService};
    use wafer_sql_utils::{introspect::build_list_tables_like, Backend};

    let Ok(url) = std::env::var(URL_ENV) else {
        eprintln!("skipping postgres two-schema resolution check: set {URL_ENV} to run");
        return;
    };
    let (admin, session, svc) = two_schema_session(&url).await;
    svc.set_strict_schema(false);

    assert_eq!(
        svc.schema_columns("conf_dup").await.expect("columns"),
        ["id", "only_first"],
        "the columns of the table the statements reach"
    );
    let err = svc
        .list(
            "conf_dup",
            &ListOptions {
                filters: vec![Filter {
                    field: "only_second".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!("x"),
                }],
                ..Default::default()
            },
        )
        .await
        .expect_err("the shadowed table's column is not the resolved table's");
    assert!(matches!(err, DatabaseError::InvalidArgument(_)), "{err:?}");
    svc.create(
        "conf_dup",
        std::collections::HashMap::from([
            ("id".to_string(), serde_json::json!("first-1")),
            ("only_first".to_string(), serde_json::json!("y")),
        ]),
    )
    .await
    .expect("create in the resolved table");
    let listed = svc
        .list("conf_dup", &ListOptions::default())
        .await
        .expect("list");
    assert_eq!(
        listed
            .records
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        ["first-1"],
        "list reads the resolved table, not the shadowed one"
    );
    assert!(
        svc.schema_table_exists("conf_second_only")
            .await
            .expect("exists"),
        "a table only the later schema holds is reachable"
    );
    assert_eq!(svc.count("conf_second_only", &[]).await.expect("count"), 0);

    let (sql, params) = build_list_tables_like("conf_", Backend::Postgres);
    let names: Vec<String> = sqlx::query_scalar(&sql)
        .bind(params[0].as_str().expect("pattern"))
        .fetch_all(&session)
        .await
        .expect("list tables like");
    assert_eq!(
        names.iter().filter(|n| n.as_str() == "conf_dup").count(),
        1,
        "a shadowed table is not listed twice: {names:?}"
    );
    assert!(names.contains(&"conf_second_only".to_string()), "{names:?}");

    drop(svc);
    session.close().await;
    for stmt in [
        "DROP SCHEMA conf_sp_first CASCADE",
        "DROP SCHEMA conf_sp_second CASCADE",
    ] {
        sqlx::query(stmt).execute(&admin).await.expect(stmt);
    }
}

/// Identity keys (`GENERATED ALWAYS` and `BY DEFAULT AS IDENTITY`) number
/// their rows as `SERIAL` does: `create` without an id gets the assigned
/// integer, which `get` finds. An integer key with no default is the
/// caller's to supply: a row without one is refused, not given a minted
/// string. Skipped unless `WAFER_CONFORMANCE_POSTGRES_URL` is set.
#[tokio::test]
async fn identity_keys_number_rows_and_a_plain_integer_key_is_the_callers() {
    use std::collections::HashMap;

    use sqlx::postgres::PgPool;
    use wafer_core::interfaces::database::service::{DatabaseError, DatabaseService};

    let Ok(url) = std::env::var(URL_ENV) else {
        eprintln!("skipping postgres identity key check: set {URL_ENV} to run");
        return;
    };
    let admin = PgPool::connect(&url).await.expect("connect as admin");
    for stmt in [
        "DROP TABLE IF EXISTS conf_identity_always, conf_identity_default, conf_plain_int",
        "CREATE TABLE conf_identity_always \
         (id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, name TEXT)",
        "CREATE TABLE conf_identity_default \
         (id INTEGER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, name TEXT)",
        "CREATE TABLE conf_plain_int (id INTEGER PRIMARY KEY, name TEXT)",
    ] {
        sqlx::query(stmt)
            .execute(&admin)
            .await
            .unwrap_or_else(|e| panic!("{stmt}: {e}"));
    }
    let svc = PostgresDatabaseService::connect(&url)
        .await
        .expect("service");
    let named = |name: &str| HashMap::from([("name".to_string(), serde_json::json!(name))]);

    for table in ["conf_identity_always", "conf_identity_default"] {
        let first = svc.create(table, named("a")).await.expect("create");
        let second = svc.create(table, named("b")).await.expect("create");
        assert_eq!(first.data["id"], serde_json::json!(1), "{table}: {first:?}");
        assert_eq!(second.id, "2", "{table}");
        let got = svc.get(table, &second.id).await.expect("get");
        assert_eq!(got.data["name"], serde_json::json!("b"));
    }

    let err = svc
        .create("conf_plain_int", named("a"))
        .await
        .expect_err("an integer key nothing fills is the caller's");
    assert!(
        matches!(&err, DatabaseError::InvalidArgument(m) if m.contains("conf_plain_int")),
        "{err:?}"
    );
    let mut with_id = named("a");
    with_id.insert("id".to_string(), serde_json::json!(5));
    let created = svc
        .create("conf_plain_int", with_id)
        .await
        .expect("supplied id");
    assert_eq!(created.id, "5");

    sqlx::query("DROP TABLE conf_identity_always, conf_identity_default, conf_plain_int")
        .execute(&admin)
        .await
        .expect("drop");
}
