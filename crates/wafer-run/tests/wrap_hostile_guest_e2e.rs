//! Task 8 hostile-guest end-to-end test (SP-A Stage 1 exit criterion).
//!
//! A REAL wasm guest (`tests/hostile_db_guest/`), compiled against the
//! PUBLIC `wafer-sdk` (not `wafer-core`'s WRAP-meta-stamping clients),
//! calls `database.exec_raw` / `database.query_raw` with no WRAP meta at
//! all — the exact meta-omission shape SP-A closes — dispatched through the
//! REAL `Wafer` runtime (wasmi) into the REAL `wafer-core` database handler
//! (`decode_and_authorize` / `RuntimeContext::check_resource_access`)
//! backed by a REAL `SQLiteDatabaseService` (in-memory).
//!
//! ## Why this crosses the wafer-run/wafer-core boundary (and how)
//!
//! wafer-run does not depend on wafer-core in its normal dependency graph
//! (wafer-core depends downward on wafer-block only). But `wafer-run`
//! already dev-depends on `wafer-block-postgres` and `wafer-block-s3` —
//! both of which themselves depend on `wafer-core` — purely for test
//! fixtures, with no cycle (wafer-core never depends back on wafer-run).
//! This test adds `wafer-core` + `wafer-block-sqlite` (in-memory, no
//! external DB server needed, unlike postgres/s3) to `[dev-dependencies]`
//! on the same basis: test-only, one-directional, precedented.
//!
//! This is therefore a genuine end-to-end test, not a documented-skip: the
//! host side under test — `wafer_core::interfaces::database::handler::
//! handle_message` calling `ctx.check_resource_access` — is the exact
//! production code, wired to a real backing store, driven by a real
//! compiled wasm guest through the real wasmi host.
//!
//! ## What's asserted
//!
//! 1. **Denial**: both ops return `PermissionDenied` to the guest — the
//!    unprivileged guest holds no `wrap_admin_block` identity and no grant,
//!    and raw SQL (`query_raw`/`exec_raw`) is admin-only regardless of any
//!    meta the guest did or didn't set.
//! 2. **Non-execution**: after the denied `CREATE TABLE` attempt, the table
//!    genuinely does not exist in the real SQLite database — checked via a
//!    direct host-side call to the same `SQLiteDatabaseService` instance
//!    the block wraps (bypassing WRAP deliberately, the way a test oracle
//!    or the runtime's own migration code legitimately would). This is
//!    real database state, not a call-log stand-in.

#![cfg(feature = "wasm")]

use std::{path::PathBuf, sync::Arc};

use wafer_block::{streams::input::InputStream, Message};
use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::{
    interfaces::database::service::DatabaseService, service_blocks::database::register_with_tables,
};
use wafer_run::{wasm::WasmiBlock, Wafer};

/// Path to the prebuilt hostile-guest wasm. Build it with:
///
/// ```bash
/// cargo build --target wasm32-wasip1 --release \
///     --manifest-path crates/wafer-run/tests/hostile_db_guest/Cargo.toml
/// ```
fn hostile_db_guest_wasm() -> Vec<u8> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/hostile_db_guest/target/wasm32-wasip1/release/hostile_db_guest.wasm");
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "failed to read hostile-db-guest wasm at {}: {e}\n\
             Did you build it first?\n  cargo build --target wasm32-wasip1 --release \\\n    \
             --manifest-path crates/wafer-run/tests/hostile_db_guest/Cargo.toml",
            p.display()
        )
    })
}

/// Build a `Wafer` with the REAL `wafer-run/database` block (real handler,
/// real in-memory SQLite backend) and the hostile guest registered as
/// `test/hostile-db-guest`, with no admin block and no WRAP grants for the
/// guest — i.e. an ordinary, unprivileged registered block.
async fn build_wafer_with_real_db() -> (Arc<Wafer>, Arc<SQLiteDatabaseService>) {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");

    let sqlite = Arc::new(SQLiteDatabaseService::open_in_memory().expect("open in-memory sqlite"));
    register_with_tables(&mut wafer, sqlite.clone(), vec![])
        .expect("register real wafer-run/database block");

    let wasm = hostile_db_guest_wasm();
    let block = WasmiBlock::load_from_bytes(&wasm).expect("load hostile-db-guest wasm");
    wafer
        .register_block("test/hostile-db-guest", Arc::new(block))
        .expect("register hostile-db-guest");

    // Deliberately no `set_admin_block`, no `ResourceGrant` for
    // "test/hostile-db-guest" — this is the baseline unprivileged caller.
    let wafer = wafer.start().await.expect("start runtime");
    (wafer, sqlite)
}

#[tokio::test]
async fn hostile_guest_exec_raw_is_denied_and_never_executes() {
    let (wafer, sqlite) = build_wafer_with_real_db().await;

    // Sanity: the table the guest tries to create must not already exist,
    // or a false pass would be possible (denial "succeeding" vacuously).
    assert!(
        !sqlite
            .schema_table_exists("test_org__hostile_guest__evil")
            .await
            .expect("schema_table_exists should not error on a fresh db"),
        "test setup invariant: the table must not exist before the exploit attempt"
    );

    let out = wafer
        .run_block(
            "test/hostile-db-guest",
            Message::new("test.exec_raw_evil"),
            InputStream::empty(),
        )
        .await;

    match out.collect_buffered().await {
        Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => {
            assert_eq!(
                e.code,
                wafer_block::ErrorCode::PermissionDenied,
                "expected PermissionDenied, got {:?}: {}",
                e.code,
                e.message
            );
        }
        other => {
            panic!("REGRESSION: hostile guest's meta-omitted exec_raw was not denied: {other:?}")
        }
    }

    // Non-execution: real DB state, not a call-log stand-in. If SP-A's
    // host-side check_resource_access were skipped or bypassable (the F7
    // vulnerability this whole program closes), this table would now exist.
    assert!(
        !sqlite
            .schema_table_exists("test_org__hostile_guest__evil")
            .await
            .expect("schema_table_exists should not error"),
        "REGRESSION: the denied CREATE TABLE actually executed against the \
         real database — the guest sandbox escape (F7) is NOT closed"
    );
}

#[tokio::test]
async fn hostile_guest_query_raw_foreign_collection_is_denied() {
    let (wafer, _sqlite) = build_wafer_with_real_db().await;

    let out = wafer
        .run_block(
            "test/hostile-db-guest",
            Message::new("test.query_raw_secrets"),
            InputStream::empty(),
        )
        .await;

    match out.collect_buffered().await {
        Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => {
            assert_eq!(
                e.code,
                wafer_block::ErrorCode::PermissionDenied,
                "expected PermissionDenied, got {:?}: {}",
                e.code,
                e.message
            );
        }
        other => panic!(
            "REGRESSION: hostile guest's meta-omitted query_raw against a \
             foreign collection was not denied: {other:?}"
        ),
    }
}
