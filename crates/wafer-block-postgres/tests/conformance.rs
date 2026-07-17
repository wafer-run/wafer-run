//! Runs the shared, backend-agnostic [`DatabaseService`] conformance suite
//! against the PostgreSQL backend.
//!
//! Unlike the SQLite suite (which uses an in-memory database and always runs),
//! this needs a **live** PostgreSQL server, so it is gated behind the
//! `WAFER_CONFORMANCE_POSTGRES_URL` environment variable. When the variable is
//! unset the test is a no-op that prints a skip notice — so a default
//! `cargo test` / CI run stays green without a database.
//!
//! To run it against a throwaway database:
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
//! NOTE: against a live server this test currently FAILS — the first live-DB
//! run of the Postgres backend surfaced real, pre-existing backend bugs
//! (windowed-counter upsert emits an ambiguous column reference; aggregate
//! `CaseWhenSum` silently decodes to NULL). Those need production fixes and are
//! out of scope for this test-only change; see the `conformance` module's
//! "Known backend divergences" section. The test is gated off by default, so
//! `cargo test --workspace` stays green regardless.
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
