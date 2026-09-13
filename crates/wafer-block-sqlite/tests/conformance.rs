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

/// Unique on-disk DB path; minimal tempfile stand-in (no new dev-dep) — the
/// same pattern `wafer-block-sqlite`'s own `service.rs` unit tests use for
/// their file-backed cases.
fn tempdb_path(tag: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!(
        "wafer-sqlite-conformance-{tag}-{}-{nonce}.db",
        std::process::id()
    ))
}

/// Same suite, same assertions, but against a **file-backed** service with
/// real read-only reader connections — the split-connection configuration
/// every production file-backed deployment runs, and the one
/// `sqlite_database_service_is_conformant` above never exercises:
/// `open_in_memory()` opens zero readers, so every "read" op there falls
/// back to the write worker and the read/write split never actually
/// happens. That gap is exactly how the `take_where` read/write-path bug
/// (`DELETE … RETURNING` dispatched through the read-only path) shipped
/// with a fully green conformance suite — the suite was never run against
/// the configuration the bug lived in. Asserting `reader_count() > 0`
/// guards against this test silently degrading back to that gap (e.g. if
/// reader connections fail to open in some environment).
#[tokio::test]
async fn sqlite_database_service_is_conformant_file_backed() {
    let path = tempdb_path("conformance");
    let svc = SQLiteDatabaseService::open(path.to_str().unwrap()).expect("open file-backed sqlite");
    assert!(
        svc.reader_count() > 0,
        "file-backed service must open reader workers, or this test exercises \
         the same single-connection configuration as the in-memory suite above"
    );
    run_conformance(&svc).await;
    let _ = std::fs::remove_file(&path);
}
