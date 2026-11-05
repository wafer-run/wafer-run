//! How an init failure is classified, and how the runtime reports it.
//!
//! - `ConfigError::SourceFault` is a permanent init failure, cached; a
//!   `ConfigError::Transient` is not.
//! - The `ResourceExhausted` rule: a `lifecycle(Init)` refused by the REAL
//!   `wafer-run/database` block because the invocation has no database call
//!   left (`CALL_BUDGET_EXHAUSTED`) is transient, and a fresh invocation
//!   runs it; a bare `ResourceExhausted` (the runtime's own limits) is
//!   permanent. A call budget has no permanent counterpart: a call is one
//!   round trip however many statements it carries, so no single call can
//!   exceed a fresh invocation's limit.
//! - `ValidationReport::unavailable` carries a source failure as a typed
//!   entry, never as a pseudo missing key.
//! - `Wafer::init_states` and `Context::block_init_states` report each
//!   block's init state without running or waiting for an init.
//!
//! Every init runs through `Wafer::init_block` (the eager-boot entry) or a
//! dispatch, so the classification under test is the pipeline's, not a
//! direct call of the classifier.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, LifecycleType, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    wire::database::DATABASE_UNAVAILABLE,
    Block, BlockInfo, ConfigVar, ErrorCode,
};
use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::{
    interfaces::database::service::{
        pk, Column, DataType, DatabaseError, DatabaseService, Table, WriteOp, WriteOutcome,
    },
    service_blocks::database::register_with_tables,
};
use wafer_run::{
    runtime::slot::TRANSIENT_RETRY_BASE, BlockInitState, ConfigError, ConfigSource, Context,
    EnvBlockConfig, InitError, TransientFailure, UnavailableBlock, UnavailableReason, Wafer,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// What [`ScriptedSource`] answers for a block.
#[derive(Clone, Copy, Debug)]
enum Answer {
    Ok,
    Transient,
    SourceFault,
    Missing,
    /// Never answers.
    Hang,
}

/// A config source answering per block, counting its loads.
#[derive(Default)]
struct ScriptedSource {
    answers: parking_lot::Mutex<HashMap<String, Answer>>,
    loads: AtomicUsize,
}

impl ScriptedSource {
    fn answer(&self, block: &str, answer: Answer) {
        self.answers.lock().insert(block.to_string(), answer);
    }
}

#[derive(Debug)]
struct SourceErr(&'static str);
impl std::fmt::Display for SourceErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for SourceErr {}

#[async_trait]
impl ConfigSource for ScriptedSource {
    async fn load_for_block(
        &self,
        block: &str,
        declared: &[ConfigVar],
    ) -> Result<EnvBlockConfig, ConfigError> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        let answer = self
            .answers
            .lock()
            .get(block)
            .copied()
            .unwrap_or(Answer::Ok);
        match answer {
            Answer::Ok => Ok(EnvBlockConfig::default()),
            // A database-backed source passes its error's detail code: this
            // read met the database's outage.
            Answer::Transient => Err(ConfigError::Transient {
                block: block.to_string(),
                source: Box::new(SourceErr("D1_ERROR: daily row read limit exceeded")),
                detail_code: Some(DATABASE_UNAVAILABLE.to_string()),
            }),
            Answer::SourceFault => Err(ConfigError::SourceFault {
                block: block.to_string(),
                source: Box::new(SourceErr("no such table: variables")),
            }),
            Answer::Hang => std::future::pending().await,
            Answer::Missing => Err(ConfigError::MissingRequired {
                block: block.to_string(),
                key: declared
                    .first()
                    .map_or_else(|| "UNDECLARED".to_string(), |v| v.key.clone()),
            }),
        }
    }
}

/// What [`Probe`]'s `lifecycle(Init)` does.
enum InitBehaviour {
    /// Succeed.
    Ok,
    /// Fail with this error.
    Fail(WaferError),
    /// Write `rows` rows to its own collection through the REAL
    /// `wafer-run/database` block, and fail with the error that answers.
    WriteRows(usize),
    /// The same, but a failed write is wrapped in an `Unavailable` of the
    /// block's own, which carries no detail code.
    WriteRowsWrapped(usize),
    /// Write `rows` rows, then fail with this error of the block's own.
    WriteRowsThenFail(usize, WaferError),
    /// Try to write `rows` rows, carry on whatever the database answers, and
    /// fail with this error of the block's own.
    TryWriteRowsThenFail(usize, WaferError),
    /// Dispatch to [`Writer`] (`test/writer`), which writes a row, and wrap a
    /// failure in an `Unavailable` of the block's own.
    CallWriter,
    /// Wait until `release` is set, then succeed.
    Gated {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    },
}

/// A block that counts its Inits and answers `handle` with a JSON map of
/// `ctx.block_init_states()` (block → state name).
struct Probe {
    name: &'static str,
    inits: Arc<AtomicUsize>,
    behaviour: InitBehaviour,
    /// The probe's declared init budget.
    init_timeout: Option<Duration>,
}

impl Probe {
    fn new(name: &'static str, behaviour: InitBehaviour) -> (Arc<Self>, Arc<AtomicUsize>) {
        let inits = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(Self {
                name,
                inits: inits.clone(),
                behaviour,
                init_timeout: None,
            }),
            inits,
        )
    }
}

/// The one config key a probe named `block` declares: `{BLOCK}__KEY`, under
/// the block's required prefix.
fn declared_key(block: &str) -> String {
    let mut key = String::new();
    for ch in block.chars() {
        match ch {
            '/' => key.push_str("__"),
            '-' => key.push('_'),
            c => key.extend(c.to_uppercase()),
        }
    }
    key.push_str("__KEY");
    key
}

/// `test/budget-init`'s own collection.
const OWN_COLLECTION: &str = "test__budget_init__items";

/// [`Writer`]'s name, and its own collection.
const WRITER: &str = "test/writer";
const WRITER_COLLECTION: &str = "test__writer__items";

/// Write `rows` rows to `collection` through the REAL `wafer-run/database`
/// block, propagating its error.
async fn write_rows_to(ctx: &dyn Context, collection: &str, rows: usize) -> Result<(), WaferError> {
    let rows = (0..rows)
        .map(|i| HashMap::from([("name".to_string(), serde_json::json!(i))]))
        .collect();
    wafer_core::clients::database::create_many(ctx, collection, rows)
        .await
        .map(|_| ())
}

/// [`write_rows_to`] `test/budget-init`'s own collection.
async fn write_rows(ctx: &dyn Context, rows: usize) -> Result<(), WaferError> {
    write_rows_to(ctx, OWN_COLLECTION, rows).await
}

/// A block whose `handle` writes one row, answering the database's error
/// when it fails. Another block's Init dispatches to it: its call is made in
/// that Init's attempt.
struct Writer;

#[async_trait]
impl Block for Writer {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(WRITER, "0.1.0", "test/iface@v1", "writes a row")
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> Result<(), WaferError> {
        Ok(())
    }

    async fn handle(&self, ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        match write_rows_to(ctx, WRITER_COLLECTION, 1).await {
            Ok(()) => OutputStream::respond(Vec::new()),
            Err(e) => OutputStream::error(e),
        }
    }
}

#[async_trait]
impl Block for Probe {
    fn info(&self) -> BlockInfo {
        let info = BlockInfo::new(
            self.name,
            "0.1.0",
            "test/iface@v1",
            "init classification probe",
        )
        .config_keys(vec![ConfigVar::new(&declared_key(self.name), "", "")]);
        match self.init_timeout {
            Some(budget) => info.init_timeout(budget),
            None => info,
        }
    }

    async fn lifecycle(&self, ctx: &dyn Context, event: LifecycleEvent) -> Result<(), WaferError> {
        if event.event_type != LifecycleType::Init {
            return Ok(());
        }
        self.inits.fetch_add(1, Ordering::SeqCst);
        match &self.behaviour {
            InitBehaviour::Ok => Ok(()),
            InitBehaviour::Fail(e) => Err(e.clone()),
            InitBehaviour::WriteRows(rows) => write_rows(ctx, *rows).await,
            InitBehaviour::WriteRowsWrapped(rows) => write_rows(ctx, *rows)
                .await
                .map_err(|_| WaferError::new(ErrorCode::Unavailable, "my store is not ready")),
            InitBehaviour::WriteRowsThenFail(rows, e) => {
                write_rows(ctx, *rows).await?;
                Err(e.clone())
            }
            InitBehaviour::TryWriteRowsThenFail(rows, e) => {
                let _ = write_rows(ctx, *rows).await;
                Err(e.clone())
            }
            InitBehaviour::CallWriter => {
                let out = ctx
                    .call_block(WRITER, Message::new("write"), InputStream::empty())
                    .await;
                match out.collect_buffered().await {
                    Ok(_) => Ok(()),
                    Err(_) => Err(WaferError::new(
                        ErrorCode::Unavailable,
                        "the writer I depend on failed",
                    )),
                }
            }
            InitBehaviour::Gated { entered, release } => {
                entered.notify_one();
                release.notified().await;
                Ok(())
            }
        }
    }

    async fn handle(&self, ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        let states: BTreeMap<String, String> = ctx
            .block_init_states()
            .into_iter()
            .map(|(block, state)| (block, state.as_str().to_string()))
            .collect();
        OutputStream::respond(serde_json::to_vec(&states).expect("serialize states"))
    }
}

fn bare_wafer(source: Arc<dyn ConfigSource>) -> Wafer {
    Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .config_source(source)
        .build()
        .expect("Wafer::build")
}

/// The REAL SQLite service behind a per-invocation call limit, as a
/// Cloudflare D1 adapter enforces one: while `spent` is set the invocation
/// has no call left, so `create_many` and `batch` (one call each, whatever
/// their length) refuse with `CallBudgetExhausted` before reaching SQLite.
/// Every other op forwards.
struct Budgeted {
    inner: SQLiteDatabaseService,
    spent: AtomicBool,
    /// The database is down: every write is refused as the backend's
    /// outage, `DatabaseError::Unavailable`.
    down: AtomicBool,
    /// The database is busy: every write is refused as
    /// `DatabaseError::Contended`.
    contended: AtomicBool,
    /// The database never answers: every write waits forever.
    hang: AtomicBool,
}

impl Budgeted {
    fn inner_service(&self) -> &dyn DatabaseService {
        &self.inner
    }

    async fn spend_call(&self, what: &str) -> Result<(), DatabaseError> {
        if self.hang.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if self.contended.load(Ordering::SeqCst) {
            return Err(DatabaseError::Contended(format!(
                "{what}: database is locked"
            )));
        }
        if self.down.load(Ordering::SeqCst) {
            return Err(DatabaseError::Unavailable(format!(
                "{what}: connection to db-1.internal:5432 refused"
            )));
        }
        if self.spent.load(Ordering::SeqCst) {
            return Err(DatabaseError::CallBudgetExhausted(format!(
                "{what}: this invocation has no database call left"
            )));
        }
        Ok(())
    }
}

wafer_core::forward_database_service! {
    impl DatabaseService for Budgeted {
        forward_to inner_service();

        ops {
            get: forward,
            list: forward,
            create: forward,
            create_many: custom,
            update: forward,
            delete: forward,
            count: forward,
            sum: forward,
            query_raw: forward,
            exec_raw: forward,
            exec_raw_transaction: forward,
            delete_where: forward,
            delete_where_count: forward,
            take_where: forward,
            update_where: forward,
            update_where_count: forward,
            increment_field_where: forward,
            upsert: forward,
            aggregate: forward,
            batch: custom,
            insert_guarded: forward,
            update_guarded: forward,
            ensure_schema_table: forward,
            ensure_schema_tables: custom,
            schema_table_exists: forward,
            schema_columns: forward,
            schema_drop_table: forward,
            schema_add_column: forward,
            catalog_tables: forward,
            catalog_columns: forward,
            catalog_row_count: forward,
        }

        async fn create_many(
            &self,
            collection: &str,
            rows: Vec<HashMap<String, serde_json::Value>>,
        ) -> Result<i64, DatabaseError> {
            self.spend_call("create_many").await?;
            self.inner.create_many(collection, rows).await
        }

        async fn batch(&self, ops: Vec<WriteOp>) -> Result<Vec<WriteOutcome>, DatabaseError> {
            self.spend_call("batch").await?;
            self.inner.batch(ops).await
        }

        async fn ensure_schema_tables(&self, tables: &[Table]) -> Result<(), DatabaseError> {
            self.spend_call("ensure_schema_tables").await?;
            self.inner.ensure_schema_tables(tables).await
        }
    }
}

/// A runtime with the REAL `wafer-run/database` block over a [`Budgeted`]
/// SQLite service, which holds `test/budget-init`'s collection, and
/// `test/budget-init`, whose Init writes `rows` rows through it.
async fn budget_wafer(rows: usize) -> (Wafer, Arc<AtomicUsize>, Arc<Budgeted>) {
    db_wafer(InitBehaviour::WriteRows(rows), None).await
}

/// [`budget_wafer`] with `test/budget-init`'s Init doing `behaviour` within
/// `init_timeout`, and [`Writer`] registered beside it.
async fn db_wafer(
    behaviour: InitBehaviour,
    init_timeout: Option<Duration>,
) -> (Wafer, Arc<AtomicUsize>, Arc<Budgeted>) {
    db_wafer_migrating(behaviour, init_timeout, vec![]).await
}

/// [`db_wafer`] with the database block's own Init migrating `tables`: it
/// runs lazily, on the first call made to the database block.
async fn db_wafer_migrating(
    behaviour: InitBehaviour,
    init_timeout: Option<Duration>,
    tables: Vec<Table>,
) -> (Wafer, Arc<AtomicUsize>, Arc<Budgeted>) {
    let mut wafer = bare_wafer(Arc::new(ScriptedSource::default()));
    let inner = SQLiteDatabaseService::open_in_memory().expect("open in-memory sqlite");
    inner
        .ensure_schema_table(&Table {
            name: OWN_COLLECTION.to_string(),
            columns: vec![
                pk("id"),
                Column::new("name", DataType::Int).null(),
                Column::new("created_at", DataType::Text).null(),
                Column::new("updated_at", DataType::Text).null(),
            ],
            indexes: Vec::new(),
            primary_key: Vec::new(),
            unique_keys: Vec::new(),
        })
        .await
        .expect("create the probe's collection");
    inner
        .ensure_schema_table(&Table {
            name: WRITER_COLLECTION.to_string(),
            columns: vec![
                pk("id"),
                Column::new("name", DataType::Int).null(),
                Column::new("created_at", DataType::Text).null(),
                Column::new("updated_at", DataType::Text).null(),
            ],
            indexes: Vec::new(),
            primary_key: Vec::new(),
            unique_keys: Vec::new(),
        })
        .await
        .expect("create the writer's collection");
    let svc = Arc::new(Budgeted {
        inner,
        spent: AtomicBool::new(false),
        down: AtomicBool::new(false),
        contended: AtomicBool::new(false),
        hang: AtomicBool::new(false),
    });
    register_with_tables(&mut wafer, svc.clone(), tables).expect("register wafer-run/database");
    let inits = Arc::new(AtomicUsize::new(0));
    let probe = Arc::new(Probe {
        name: "test/budget-init",
        inits: inits.clone(),
        behaviour,
        init_timeout,
    });
    wafer
        .register_block("test/budget-init", probe)
        .expect("register probe");
    wafer
        .register_block(WRITER, Arc::new(Writer))
        .expect("register the writer");
    wafer.seal().await.expect("seal");
    (wafer, inits, svc)
}

async fn dispatch_error(wafer: &Wafer, block: &str) -> WaferError {
    let out = wafer
        .run_block(block, Message::new("probe"), InputStream::empty())
        .await;
    match out.collect_buffered().await {
        Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => e,
        other => panic!("dispatch to {block} must fail, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// ConfigError::SourceFault / Transient
// ---------------------------------------------------------------------------

/// A source fault is permanent: cached, the source is not asked again, and
/// the block reports `Failed`.
#[tokio::test]
async fn a_config_source_fault_is_a_cached_permanent_failure() {
    let source = Arc::new(ScriptedSource::default());
    source.answer("test/faulty", Answer::SourceFault);
    let mut wafer = bare_wafer(source.clone());
    let (probe, inits) = Probe::new("test/faulty", InitBehaviour::Ok);
    wafer
        .register_block("test/faulty", probe)
        .expect("register");
    wafer.seal().await.expect("seal");

    for attempt in 0..3 {
        let err = wafer
            .init_block("test/faulty")
            .await
            .expect_err("a source fault fails init");
        match err {
            InitError::Permanent(msg) => assert!(
                msg.contains("config source fault") && msg.contains("no such table"),
                "attempt {attempt}: {msg}"
            ),
            other => panic!("attempt {attempt}: expected Permanent, got {other:?}"),
        }
    }
    assert_eq!(
        source.loads.load(Ordering::SeqCst),
        1,
        "a permanent failure is cached: the source is read once"
    );
    assert_eq!(inits.load(Ordering::SeqCst), 0, "Init never ran");
    assert!(matches!(
        wafer.init_states().get("test/faulty"),
        Some(BlockInitState::Failed { reason }) if reason.contains("config source fault")
    ));

    let err = dispatch_error(&wafer, "test/faulty").await;
    assert_eq!(err.code, ErrorCode::FailedPrecondition, "{err:?}");
}

/// A transient source error is not cached: past the backoff the source is
/// read again, and once it answers the block is `Ready`.
#[tokio::test]
async fn a_transient_config_error_is_not_cached() {
    let source = Arc::new(ScriptedSource::default());
    source.answer("test/flaky", Answer::Transient);
    let mut wafer = bare_wafer(source.clone());
    let (probe, inits) = Probe::new("test/flaky", InitBehaviour::Ok);
    wafer.register_block("test/flaky", probe).expect("register");
    wafer.seal().await.expect("seal");

    let err = wafer.init_block("test/flaky").await.expect_err("transient");
    assert!(
        matches!(&err, InitError::Transient(failure)
            if failure.message.contains("config fetch failed")
                && failure.detail_code.as_deref() == Some(DATABASE_UNAVAILABLE)),
        "the source's detail code is carried: {err:?}"
    );
    match wafer.init_states().get("test/flaky") {
        Some(BlockInitState::Retrying { failure, retry_in }) => {
            let reason = &failure.message;
            assert!(reason.contains("daily row read limit"), "{reason}");
            assert_eq!(
                failure.detail_code.as_deref(),
                Some(DATABASE_UNAVAILABLE),
                "the slot keeps it"
            );
            // At most the first backoff; it may already have elapsed on a
            // slow runner (then zero), so no lower bound is asserted.
            assert!(*retry_in <= TRANSIENT_RETRY_BASE, "{retry_in:?}");
        }
        other => panic!("expected Retrying after a transient failure, got {other:?}"),
    }

    tokio::time::sleep(TRANSIENT_RETRY_BASE * 2).await;
    assert!(
        matches!(
            wafer.init_states().get("test/flaky"),
            Some(BlockInitState::Retrying { retry_in, .. }) if *retry_in == Duration::ZERO
        ),
        "past the backoff the last failure is still reported, due now"
    );
    source.answer("test/flaky", Answer::Ok);
    wafer
        .init_block("test/flaky")
        .await
        .expect("retried and succeeded");
    assert_eq!(
        source.loads.load(Ordering::SeqCst),
        2,
        "the source was read again"
    );
    assert_eq!(inits.load(Ordering::SeqCst), 1);
    assert_eq!(
        wafer.init_states().get("test/flaky"),
        Some(&BlockInitState::Ready)
    );
}

// ---------------------------------------------------------------------------
// The ResourceExhausted rule, through the real database block
// ---------------------------------------------------------------------------

/// An Init whose write meets an invocation with no database call left: the
/// database answers `ResourceExhausted` + `CALL_BUDGET_EXHAUSTED`, and the
/// Init fails transiently, nothing written, `Retrying`. Once a fresh
/// invocation has calls again, the re-run past the backoff writes every row
/// in its one call and the block is `Ready`.
#[tokio::test]
async fn an_init_that_meets_a_spent_call_budget_fails_transiently_and_reruns() {
    let (wafer, inits, svc) = budget_wafer(3).await;
    svc.spent.store(true, Ordering::SeqCst);
    let err = wafer
        .init_block("test/budget-init")
        .await
        .expect_err("the call budget refuses the write");
    match err {
        InitError::Transient(TransientFailure { message: msg, .. }) => {
            assert!(
                msg.contains("lifecycle init failed") && msg.contains("ResourceExhausted"),
                "the database's own refusal: {msg}"
            );
        }
        other => panic!("expected Transient, got {other:?}"),
    }
    assert_eq!(inits.load(Ordering::SeqCst), 1);
    assert!(matches!(
        wafer.init_states().get("test/budget-init"),
        Some(BlockInitState::Retrying { .. })
    ));
    assert_eq!(
        svc.inner.count(OWN_COLLECTION, &[]).await.expect("count"),
        0,
        "the refused call wrote nothing"
    );

    // A fresh invocation: calls again.
    svc.spent.store(false, Ordering::SeqCst);
    tokio::time::sleep(TRANSIENT_RETRY_BASE * 2).await;
    wafer
        .init_block("test/budget-init")
        .await
        .expect("the re-run writes its rows");
    assert_eq!(inits.load(Ordering::SeqCst), 2, "Init ran again");
    assert_eq!(
        wafer.init_states().get("test/budget-init"),
        Some(&BlockInitState::Ready)
    );
    assert_eq!(
        svc.inner.count(OWN_COLLECTION, &[]).await.expect("count"),
        3
    );
}

/// The same through a database outage: the real `wafer-run/database` block
/// answers the backend's `DatabaseError::Unavailable` as `Unavailable` with
/// the `database.unavailable` detail code; the client hands it to the
/// block's `lifecycle(Init)`, which propagates it with `?`; the Init fails
/// transiently, and the block's slot keeps the detail code — in the error and
/// in its `Retrying` state — through the backoff. Back up, the re-run writes
/// its rows.
#[tokio::test]
async fn an_init_that_meets_a_database_outage_keeps_its_detail_code_and_reruns() {
    let (wafer, inits, svc) = budget_wafer(3).await;
    svc.down.store(true, Ordering::SeqCst);
    let err = wafer
        .init_block("test/budget-init")
        .await
        .expect_err("the database is down");
    match &err {
        InitError::Transient(failure) => {
            assert_eq!(
                failure.detail_code.as_deref(),
                Some(DATABASE_UNAVAILABLE),
                "{failure:?}"
            );
            assert!(
                !failure.message.contains("db-1.internal"),
                "the driver's text is scrubbed: {failure:?}"
            );
        }
        other => panic!("expected Transient, got {other:?}"),
    }
    match wafer.init_states().get("test/budget-init") {
        Some(BlockInitState::Retrying { failure, .. }) => assert_eq!(
            failure.detail_code.as_deref(),
            Some(DATABASE_UNAVAILABLE),
            "{failure:?}"
        ),
        other => panic!("expected Retrying, got {other:?}"),
    }

    svc.down.store(false, Ordering::SeqCst);
    tokio::time::sleep(TRANSIENT_RETRY_BASE * 2).await;
    wafer
        .init_block("test/budget-init")
        .await
        .expect("the re-run writes its rows");
    assert_eq!(inits.load(Ordering::SeqCst), 2, "Init ran again");
    assert_eq!(
        wafer.init_states().get("test/budget-init"),
        Some(&BlockInitState::Ready)
    );
}

/// A `ResourceExhausted` without the call-budget detail code is one of the
/// runtime's own limits: permanent.
#[tokio::test]
async fn a_bare_resource_exhausted_init_failure_is_permanent() {
    let mut wafer = bare_wafer(Arc::new(ScriptedSource::default()));
    let (probe, inits) = Probe::new(
        "test/limit",
        InitBehaviour::Fail(WaferError::new(
            ErrorCode::ResourceExhausted,
            "call depth limit exceeded",
        )),
    );
    wafer.register_block("test/limit", probe).expect("register");
    wafer.seal().await.expect("seal");
    let err = wafer.init_block("test/limit").await.expect_err("fails");
    assert!(matches!(err, InitError::Permanent(_)), "{err:?}");
    wafer.init_block("test/limit").await.expect_err("cached");
    assert_eq!(inits.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// ValidationReport::unavailable
// ---------------------------------------------------------------------------

/// A block reporting `ctx.validate_all_block_configs()` as JSON.
struct Validator;

#[async_trait]
impl Block for Validator {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/validator", "0.1.0", "test/iface@v1", "validates")
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
    async fn handle(&self, ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        let report = ctx.validate_all_block_configs().await;
        let body = serde_json::json!({
            "ok": report.ok,
            "broken": report.broken.iter().map(|b| (b.block.clone(), b.missing_keys.clone())).collect::<Vec<_>>(),
            "unavailable": report.unavailable.iter().map(|u| (u.block.clone(), u.reason.as_str())).collect::<Vec<_>>(),
        });
        OutputStream::respond(serde_json::to_vec(&body).expect("json"))
    }
}

/// Each source failure is a typed `unavailable` entry with its class; a
/// missing key stays in `broken`, and no pseudo-key appears there.
#[tokio::test]
async fn validation_reports_source_failures_as_typed_unavailable_entries() {
    let source = Arc::new(ScriptedSource::default());
    source.answer("test/a-healthy", Answer::Ok);
    source.answer("test/b-missing", Answer::Missing);
    source.answer("test/c-transient", Answer::Transient);
    source.answer("test/d-fault", Answer::SourceFault);
    let mut wafer = bare_wafer(source);
    for name in [
        "test/a-healthy",
        "test/b-missing",
        "test/c-transient",
        "test/d-fault",
    ] {
        let (probe, _) = Probe::new(name, InitBehaviour::Ok);
        wafer.register_block(name, probe).expect("register");
    }
    wafer
        .register_block("test/validator", Arc::new(Validator))
        .expect("register validator");
    wafer.seal().await.expect("seal");

    let report = wafer.validate_all_block_configs().await;
    assert_eq!(
        report.unavailable,
        vec![
            // The source's detail code is carried into the report.
            UnavailableBlock {
                block: "test/c-transient".to_string(),
                reason: UnavailableReason::Transient,
                detail_code: Some(DATABASE_UNAVAILABLE.to_string()),
            },
            UnavailableBlock {
                block: "test/d-fault".to_string(),
                reason: UnavailableReason::SourceFault,
                detail_code: None,
            },
        ]
    );
    assert_eq!(report.broken.len(), 1, "{:?}", report.broken);
    assert_eq!(report.broken[0].block, "test/b-missing");
    assert_eq!(
        report.broken[0].missing_keys,
        vec!["TEST__B_MISSING__KEY".to_string()]
    );
    assert!(report.ok.contains(&"test/a-healthy".to_string()));
    assert!(!report
        .ok
        .iter()
        .any(|b| b.starts_with("test/c") || b.starts_with("test/d")));

    // The same report from inside a block.
    let out = wafer
        .run_block(
            "test/validator",
            Message::new("validate"),
            InputStream::empty(),
        )
        .await
        .collect_buffered()
        .await
        .expect("validator answers");
    let body: serde_json::Value = serde_json::from_slice(&out.body).expect("json");
    assert_eq!(
        body["unavailable"],
        serde_json::json!([
            ["test/c-transient", "transient"],
            ["test/d-fault", "source_fault"]
        ])
    );
    assert_eq!(
        body["broken"],
        serde_json::json!([["test/b-missing", ["TEST__B_MISSING__KEY"]]])
    );
}

// ---------------------------------------------------------------------------
// init_states
// ---------------------------------------------------------------------------

/// Every state, read through `Wafer::init_states` and through a block's
/// `ctx.block_init_states()`. Reading never runs an Init, aliases are not
/// listed, and an attempt in flight is `Initializing` without the read
/// waiting for it.
#[tokio::test]
async fn init_states_report_every_block_without_running_or_waiting_for_init() {
    let source = Arc::new(ScriptedSource::default());
    source.answer("test/retrying", Answer::Transient);
    source.answer("test/failed", Answer::SourceFault);
    let mut wafer = bare_wafer(source);

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut counters = HashMap::new();
    for (name, behaviour) in [
        ("test/ready", InitBehaviour::Ok),
        ("test/untouched", InitBehaviour::Ok),
        ("test/retrying", InitBehaviour::Ok),
        ("test/failed", InitBehaviour::Ok),
        (
            "test/in-flight",
            InitBehaviour::Gated {
                entered: entered.clone(),
                release: release.clone(),
            },
        ),
        ("test/reporter", InitBehaviour::Ok),
    ] {
        let (probe, inits) = Probe::new(name, behaviour);
        wafer.register_block(name, probe).expect("register");
        counters.insert(name, inits);
    }
    wafer
        .add_alias("test/ready-alias", "test/ready")
        .expect("alias");
    wafer.seal().await.expect("seal");
    let wafer = Arc::new(wafer);

    let all_not_initialized = wafer.init_states();
    assert_eq!(all_not_initialized.len(), 6, "{all_not_initialized:?}");
    assert!(all_not_initialized
        .values()
        .all(|s| *s == BlockInitState::NotInitialized));

    wafer.init_block("test/ready").await.expect("ready");
    wafer.init_block("test/reporter").await.expect("ready");
    wafer
        .init_block("test/retrying")
        .await
        .expect_err("transient");
    wafer
        .init_block("test/failed")
        .await
        .expect_err("permanent");

    // An Init in flight, parked inside its lifecycle.
    let in_flight = {
        let wafer = wafer.clone();
        tokio::spawn(async move { wafer.init_block("test/in-flight").await })
    };
    entered.notified().await;

    let states = wafer.init_states();
    assert_eq!(states.get("test/ready"), Some(&BlockInitState::Ready));
    assert_eq!(
        states.get("test/untouched"),
        Some(&BlockInitState::NotInitialized)
    );
    assert!(matches!(
        states.get("test/retrying"),
        Some(BlockInitState::Retrying { failure, .. }) if failure.message.contains("config fetch failed")
    ));
    assert!(matches!(
        states.get("test/failed"),
        Some(BlockInitState::Failed { reason }) if reason.contains("config source fault")
    ));
    assert_eq!(
        states.get("test/in-flight"),
        Some(&BlockInitState::Initializing)
    );
    assert!(
        !states.contains_key("test/ready-alias"),
        "aliases are not listed"
    );

    // The same view from inside a block, read while the Init is still in
    // flight: the dispatch answers instead of waiting for it.
    let out = tokio::time::timeout(Duration::from_secs(5), async {
        wafer
            .run_block(
                "test/reporter",
                Message::new("states"),
                InputStream::empty(),
            )
            .await
            .collect_buffered()
            .await
            .expect("reporter answers")
    })
    .await
    .expect("reading the init states never waits for an init in flight");
    let body: BTreeMap<String, String> = serde_json::from_slice(&out.body).expect("json");
    assert_eq!(
        body,
        BTreeMap::from(
            [
                ("test/failed", "failed"),
                ("test/in-flight", "initializing"),
                ("test/ready", "ready"),
                ("test/reporter", "ready"),
                ("test/retrying", "retrying"),
                ("test/untouched", "not_initialized"),
            ]
            .map(|(k, v)| (k.to_string(), v.to_string()))
        )
    );

    release.notify_one();
    in_flight
        .await
        .expect("join")
        .expect("in-flight init succeeds");
    assert_eq!(
        wafer.init_states().get("test/in-flight"),
        Some(&BlockInitState::Ready)
    );

    // Reading ran no Init: each block's count is what its init_block calls
    // made it.
    for (name, expected) in [
        ("test/ready", 1),
        ("test/untouched", 0),
        ("test/retrying", 0),
        ("test/failed", 0),
        ("test/in-flight", 1),
        ("test/reporter", 1),
    ] {
        assert_eq!(counters[name].load(Ordering::SeqCst), expected, "{name}");
    }
}

// ---------------------------------------------------------------------------
// Attempt-level attribution: what the attempt's own database calls saw
// ---------------------------------------------------------------------------

/// `test/budget-init`'s failure class after one transient Init attempt: the
/// detail code of the error `init_block` answered, which must be the one its
/// `Retrying` state keeps.
async fn attempt_class(wafer: &Wafer) -> Option<String> {
    let failure = match wafer.init_block("test/budget-init").await {
        Err(InitError::Transient(failure)) => failure,
        other => panic!("expected a transient failure, got {other:?}"),
    };
    match wafer.init_states().get("test/budget-init") {
        Some(BlockInitState::Retrying { failure: kept, .. }) => {
            assert_eq!(kept.detail_code, failure.detail_code, "the slot keeps it");
        }
        other => panic!("expected Retrying, got {other:?}"),
    }
    failure.detail_code
}

/// A block that wraps its database's error in one of its own (no detail
/// code) during an outage still reads as the database's failure: the
/// attempt's call was answered `Unavailable` where the handler answered it.
#[tokio::test]
async fn a_wrapped_error_during_an_outage_reads_as_the_database_down() {
    let (wafer, _, svc) = db_wafer(InitBehaviour::WriteRowsWrapped(1), None).await;
    svc.down.store(true, Ordering::SeqCst);
    assert_eq!(
        attempt_class(&wafer).await.as_deref(),
        Some(DATABASE_UNAVAILABLE)
    );
}

/// A block's Init that dispatches to another block, whose call meets the
/// outage, reads as the database down: the other block's call is made in the
/// Init's attempt, whatever the Init then reports.
#[tokio::test]
async fn a_nested_blocks_outage_is_attributed_to_the_outer_attempt() {
    let (wafer, _, svc) = db_wafer(InitBehaviour::CallWriter, None).await;
    svc.down.store(true, Ordering::SeqCst);
    assert_eq!(
        attempt_class(&wafer).await.as_deref(),
        Some(DATABASE_UNAVAILABLE)
    );
}

/// A call that never answers, cut off by the init budget, is the database
/// not answering: the budget overrun reads `database.unavailable`, not a
/// classless timeout.
#[tokio::test]
async fn a_hung_call_cut_off_by_the_init_budget_reads_as_the_database_down() {
    let (wafer, _, svc) = db_wafer(
        InitBehaviour::WriteRows(1),
        Some(Duration::from_millis(100)),
    )
    .await;
    svc.hang.store(true, Ordering::SeqCst);
    assert_eq!(
        attempt_class(&wafer).await.as_deref(),
        Some(DATABASE_UNAVAILABLE)
    );
}

/// A busy database is contention, not an outage, even when the block wraps
/// the error.
#[tokio::test]
async fn contention_reads_as_contention() {
    use wafer_block::wire::database::DATABASE_CONTENDED;

    let (wafer, _, svc) = db_wafer(InitBehaviour::WriteRowsWrapped(1), None).await;
    svc.contended.store(true, Ordering::SeqCst);
    assert_eq!(
        attempt_class(&wafer).await.as_deref(),
        Some(DATABASE_CONTENDED)
    );
}

/// An attempt whose every call answered, and whose block then failed on its
/// own, has no database class: the block's own failure stands.
#[tokio::test]
async fn a_block_failing_on_its_own_after_every_call_answered_has_no_class() {
    let own = WaferError::new(ErrorCode::Unavailable, "the mail relay is down");
    let (wafer, _, _) = db_wafer(InitBehaviour::WriteRowsThenFail(1, own), None).await;
    assert_eq!(attempt_class(&wafer).await, None);
}

/// One attempt's record is its own: an attempt that met the outage reads as
/// the database down, and the next one — the database back, the block
/// failing on its own — does not inherit it.
#[tokio::test]
async fn an_attempt_does_not_read_another_attempts_calls() {
    let own = WaferError::new(ErrorCode::Unavailable, "the mail relay is down");
    let (wafer, _, svc) = db_wafer(InitBehaviour::WriteRowsThenFail(1, own), None).await;
    svc.down.store(true, Ordering::SeqCst);
    assert_eq!(
        attempt_class(&wafer).await.as_deref(),
        Some(DATABASE_UNAVAILABLE)
    );
    svc.down.store(false, Ordering::SeqCst);
    tokio::time::sleep(TRANSIENT_RETRY_BASE * 2).await;
    assert_eq!(attempt_class(&wafer).await, None, "the next attempt's own");
}

/// The database block's own Init — its schema migration, started lazily by
/// the first call another block's Init makes to it — meets the outage. Its
/// call is noted in its attempt and in the attempt it runs inside, so the
/// caller, which wraps the refusal it gets, reads as the database down.
#[tokio::test]
async fn the_database_blocks_own_init_during_an_outage_reaches_the_callers_attempt() {
    let migrated = Table {
        name: "test__budget_init__migrated".to_string(),
        columns: vec![pk("id")],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    };
    let (wafer, _, svc) =
        db_wafer_migrating(InitBehaviour::WriteRowsWrapped(1), None, vec![migrated]).await;
    svc.down.store(true, Ordering::SeqCst);
    assert_eq!(
        attempt_class(&wafer).await.as_deref(),
        Some(DATABASE_UNAVAILABLE)
    );
}

/// The record never downgrades the block's own database class: an attempt
/// whose call met contention, and whose block then failed reporting the
/// database down, reads as the database down (the stronger class).
#[tokio::test]
async fn the_record_never_downgrades_the_blocks_own_database_class() {
    let down = WaferError::new(ErrorCode::Unavailable, "database temporarily unavailable")
        .with_detail_code(DATABASE_UNAVAILABLE);
    let (wafer, _, svc) = db_wafer(InitBehaviour::TryWriteRowsThenFail(1, down), None).await;
    svc.contended.store(true, Ordering::SeqCst);
    assert_eq!(
        attempt_class(&wafer).await.as_deref(),
        Some(DATABASE_UNAVAILABLE)
    );
}

/// A config load that never answers is a call of the attempt still in
/// flight when the init budget runs out: the database not answering.
#[tokio::test]
async fn a_hung_config_load_cut_off_by_the_init_budget_reads_as_the_database_down() {
    let source = Arc::new(ScriptedSource::default());
    source.answer("test/hung-config", Answer::Hang);
    let mut wafer = bare_wafer(source);
    let probe = Arc::new(Probe {
        name: "test/hung-config",
        inits: Arc::new(AtomicUsize::new(0)),
        behaviour: InitBehaviour::Ok,
        init_timeout: Some(Duration::from_millis(100)),
    });
    wafer
        .register_block("test/hung-config", probe)
        .expect("register");
    wafer.seal().await.expect("seal");
    match wafer.init_block("test/hung-config").await {
        Err(InitError::Transient(failure)) => assert_eq!(
            failure.detail_code.as_deref(),
            Some(DATABASE_UNAVAILABLE),
            "{failure:?}"
        ),
        other => panic!("expected a transient failure, got {other:?}"),
    }
}
