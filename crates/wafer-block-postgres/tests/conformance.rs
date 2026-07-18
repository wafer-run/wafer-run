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
//! This passes against a live server. The first live-DB run of the Postgres
//! backend surfaced four real, pre-existing backend bugs (timestamp string vs
//! `TIMESTAMPTZ`; `sum` over an `INT` column; windowed-counter upsert emitting
//! an ambiguous column reference; aggregate `CaseWhenSum` silently decoding to
//! NULL); all four are now fixed at the shared renderer / decoder layer and the
//! suite exercises each — see the `conformance` module's "Backend divergences"
//! section. The test is still gated off by default (no `WAFER_CONFORMANCE_POSTGRES_URL`
//! → skip), so `cargo test --workspace` stays green without a database.
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
