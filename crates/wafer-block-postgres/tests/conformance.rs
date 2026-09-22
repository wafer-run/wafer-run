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
