//! Runs the shared, backend-agnostic [`DatabaseService`] conformance suite
//! against the SQLite backend.
//!
//! This is the reference invocation of
//! [`wafer_core::interfaces::database::conformance::run_conformance`]. The same
//! call is what impresspress's D1 and browser-WASM adapters use to prove they
//! have not drifted from the native backends — if any op there silently
//! no-ops or fails open, the corresponding assertion in the shared suite
//! fails.
//!
//! [`DatabaseService`]: wafer_core::interfaces::database::service::DatabaseService

use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::interfaces::database::conformance::run_conformance;

/// The SQLite `DatabaseService` implementation must satisfy every op in the
/// shared conformance suite.
#[tokio::test]
async fn sqlite_database_service_is_conformant() {
    let svc = SQLiteDatabaseService::open_in_memory().expect("open in-memory sqlite");
    run_conformance(&svc).await;
}
