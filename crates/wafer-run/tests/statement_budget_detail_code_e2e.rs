//! A statement-budget refusal keeps its detail code on every transport.
//!
//! A REAL wasm guest (`tests/service_client_guest/`) calls
//! `wafer_core::clients::database::create_many` / `::batch` — the typed
//! clients a guest block uses — through the REAL wasmi host into the REAL
//! `wafer-run/database` block (the shared database handler over an in-memory
//! SQLite service that reports a `Limited` statement budget, as a Cloudflare
//! D1 adapter does). The guest hands the host's error back as its own, so
//! the error crosses the host → guest stream frame and the guest → host
//! `GuestResult` before the test sees it. The test then encodes that error
//! the two ways it leaves a runtime: the HTTP error body
//! (`http_codec::error_to_http_response`, which every HTTP adapter uses) and
//! the embedder wire format (`embed::output_to_json`, which the C ABI's
//! `wafer_run` and the Node addon's `run` return verbatim).
//!
//! The coarse codes are shared with other refusals (`InvalidArgument` with
//! any malformed request, `ResourceExhausted` with rate limits and the
//! call-depth limit), so the detail code is the only thing a caller can key
//! "do not retry this request as is" on.

#![cfg(feature = "wasm")]

use std::{path::PathBuf, sync::Arc};

use wafer_block::{
    http_codec::error_to_http_response,
    streams::{input::InputStream, output::TerminalNotResponse},
    wire::database::{STATEMENT_BUDGET_EXCEEDS_LIMIT, STATEMENT_BUDGET_EXHAUSTED},
    ErrorCode, Message, OutputStream, WaferError,
};
use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::{
    interfaces::database::service::{DatabaseError, DatabaseService, StatementBudget},
    service_blocks::database::register_with_tables,
};
use wafer_run::{embed::output_to_json, wasm::WasmiBlock, Wafer};

const GUEST: &str = "test/service-client-guest";

/// Path to the prebuilt service-client guest wasm (`scripts/build-fixtures.sh`
/// builds it).
fn service_client_guest_wasm() -> Vec<u8> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/service_client_guest/target/wasm32-wasip1/release/service_client_guest.wasm");
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "failed to read service-client guest wasm at {}: {e}\n\
             Did you build it first?\n  cargo build --target wasm32-wasip1 --release \\\n    \
             --manifest-path crates/wafer-run/tests/service_client_guest/Cargo.toml",
            p.display()
        )
    })
}

/// The real SQLite service behind a budget it does not have: every op is
/// forwarded except `statement_budget`, which reports `budget`.
struct Budgeted {
    inner: SQLiteDatabaseService,
    budget: StatementBudget,
}

impl Budgeted {
    fn inner_service(&self) -> &dyn DatabaseService {
        &self.inner
    }
}

wafer_core::forward_database_service! {
    impl DatabaseService for Budgeted {
        forward_to inner_service();

        ops {
            get: forward,
            list: forward,
            create: forward,
            create_many: forward,
            update: forward,
            delete: forward,
            count: forward,
            sum: forward,
            query_raw: forward,
            exec_raw: forward,
            delete_where: forward,
            delete_where_count: forward,
            take_where: forward,
            update_where: forward,
            update_where_count: forward,
            increment_field_where: forward,
            upsert: forward,
            aggregate: forward,
            batch: forward,
            insert_guarded: forward,
            update_guarded: forward,
            ensure_schema_table: forward,
            ensure_schema_tables: forward,
            schema_table_exists: forward,
            schema_columns: forward,
            schema_drop_table: forward,
            schema_add_column: forward,
            set_strict_schema: forward,
            statement_budget: custom,
        }

        fn statement_budget(&self) -> Result<StatementBudget, DatabaseError> {
            Ok(self.budget)
        }
    }
}

/// A runtime whose `wafer-run/database` runs at most `limit` statements per
/// invocation and has already run `used`, with the guest registered.
async fn wafer_with_budget(limit: u64, used: u64) -> Arc<Wafer> {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");
    let svc = Budgeted {
        inner: SQLiteDatabaseService::open_in_memory().expect("open in-memory sqlite"),
        budget: StatementBudget::Limited { limit, used },
    };
    register_with_tables(&mut wafer, Arc::new(svc), vec![]).expect("register wafer-run/database");
    let block = WasmiBlock::load_approving_declaration(
        &service_client_guest_wasm(),
        wafer_run::ResourceLimits::default(),
    )
    .expect("load service-client guest wasm");
    wafer
        .register_block(GUEST, Arc::new(block))
        .expect("register service-client-guest");
    wafer.start().await.expect("start runtime")
}

/// Have the guest write `rows` rows through its `kind` client call, and
/// return the error it ends with.
async fn guest_error(wafer: &Wafer, kind: &str, rows: usize) -> WaferError {
    let out = wafer
        .run_block(
            GUEST,
            Message::new(kind),
            InputStream::from_bytes(rows.to_string().into_bytes()),
        )
        .await;
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => e,
        other => panic!("{kind}: {rows} rows must be refused by the budget, got {other:?}"),
    }
}

/// `err`, which the guest ended with, carries `detail` as it arrived and
/// through both encodings a runtime answers with.
async fn assert_detail_on_every_transport(
    kind: &str,
    err: WaferError,
    code: ErrorCode,
    status: u16,
    detail: &str,
) {
    // Host → guest stream frame, then guest → host `GuestResult`.
    assert_eq!(err.code, code, "{kind}: {}", err.message);
    assert_eq!(
        err.detail_code(),
        Some(detail),
        "{kind}: the guest wire kept the detail code: {err:?}"
    );

    // HTTP: the JSON error body every HTTP adapter sends.
    let http = error_to_http_response(&err);
    assert_eq!(http.status, status, "{kind}");
    let body: serde_json::Value = serde_json::from_slice(&http.body).expect("JSON error body");
    assert_eq!(body["code"], detail, "{kind}: HTTP error body {body}");

    // Embedder wire format: what the C ABI and the Node addon return.
    let embedded: serde_json::Value =
        serde_json::from_str(&output_to_json(OutputStream::error(err)).await)
            .expect("embedder JSON");
    assert_eq!(embedded["action"], "error", "{kind}: {embedded}");
    assert_eq!(
        embedded["error"]["detail_code"], detail,
        "{kind}: embedder JSON {embedded}"
    );
}

/// Six statements with five of the invocation's twenty left: fits the limit,
/// not what is left.
#[tokio::test]
async fn an_exhausted_budget_carries_its_detail_code_on_every_transport() {
    let wafer = wafer_with_budget(20, 15).await;
    for kind in ["test.db_create_many", "test.db_batch"] {
        let err = guest_error(&wafer, kind, 6).await;
        assert_detail_on_every_transport(
            kind,
            err,
            ErrorCode::ResourceExhausted,
            429,
            STATEMENT_BUDGET_EXHAUSTED,
        )
        .await;
    }
}

/// Six statements against a limit of five: no invocation can run it.
#[tokio::test]
async fn a_write_over_the_whole_limit_carries_its_detail_code_on_every_transport() {
    let wafer = wafer_with_budget(5, 0).await;
    for kind in ["test.db_create_many", "test.db_batch"] {
        let err = guest_error(&wafer, kind, 6).await;
        assert_detail_on_every_transport(
            kind,
            err,
            ErrorCode::InvalidArgument,
            400,
            STATEMENT_BUDGET_EXCEEDS_LIMIT,
        )
        .await;
    }
}
