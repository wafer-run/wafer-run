//! How an init failure is classified, and how the runtime reports it.
//!
//! - `ConfigError::SourceFault` is a permanent init failure, cached; a
//!   `ConfigError::Transient` is not.
//! - The `ResourceExhausted` rule: a `lifecycle(Init)` refused by the REAL
//!   `wafer-run/database` block because the invocation's statement budget
//!   is spent (`STATEMENT_BUDGET_EXHAUSTED`) is transient; one refused
//!   because no invocation can run it (`StatementLimitExceeded`) and a bare
//!   `ResourceExhausted` (the runtime's own limits) are permanent.
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
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, LifecycleType, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    Block, BlockInfo, ConfigVar, ErrorCode,
};
use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::{
    interfaces::database::service::{DatabaseError, DatabaseService, StatementBudget},
    service_blocks::database::register_with_tables,
};
use wafer_run::{
    runtime::slot::TRANSIENT_RETRY_BASE, BlockInitState, ConfigError, ConfigSource, Context,
    EnvBlockConfig, InitError, UnavailableBlock, UnavailableReason, Wafer,
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
            Answer::Transient => Err(ConfigError::Transient {
                block: block.to_string(),
                source: Box::new(SourceErr("D1_ERROR: daily row read limit exceeded")),
            }),
            Answer::SourceFault => Err(ConfigError::SourceFault {
                block: block.to_string(),
                source: Box::new(SourceErr("no such table: variables")),
            }),
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
}

impl Probe {
    fn new(name: &'static str, behaviour: InitBehaviour) -> (Arc<Self>, Arc<AtomicUsize>) {
        let inits = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(Self {
                name,
                inits: inits.clone(),
                behaviour,
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

#[async_trait]
impl Block for Probe {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            self.name,
            "0.1.0",
            "test/iface@v1",
            "init classification probe",
        )
        .config_keys(vec![ConfigVar::new(&declared_key(self.name), "", "")])
    }

    async fn lifecycle(&self, ctx: &dyn Context, event: LifecycleEvent) -> Result<(), WaferError> {
        if event.event_type != LifecycleType::Init {
            return Ok(());
        }
        self.inits.fetch_add(1, Ordering::SeqCst);
        match &self.behaviour {
            InitBehaviour::Ok => Ok(()),
            InitBehaviour::Fail(e) => Err(e.clone()),
            InitBehaviour::WriteRows(rows) => {
                let rows = (0..*rows)
                    .map(|i| HashMap::from([("name".to_string(), serde_json::json!(i))]))
                    .collect();
                wafer_core::clients::database::create_many(ctx, OWN_COLLECTION, rows)
                    .await
                    .map(|_| ())
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

/// The REAL SQLite service behind a statement budget it does not have:
/// every op is forwarded except `statement_budget`, which reports `budget`
/// (as a Cloudflare D1 adapter reports its per-invocation limit).
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
            exec_raw_transaction: forward,
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
            statement_budget: custom,
        }

        fn statement_budget(&self) -> Result<StatementBudget, DatabaseError> {
            Ok(self.budget)
        }
    }
}

/// A runtime with the REAL `wafer-run/database` block over a service that
/// runs at most `limit` statements per invocation and has run `used`, and
/// `test/budget-init`, whose Init writes `rows` rows through it.
async fn budget_wafer(limit: u64, used: u64, rows: usize) -> (Wafer, Arc<AtomicUsize>) {
    let mut wafer = bare_wafer(Arc::new(ScriptedSource::default()));
    let svc = Budgeted {
        inner: SQLiteDatabaseService::open_in_memory().expect("open in-memory sqlite"),
        budget: StatementBudget::Limited { limit, used },
    };
    register_with_tables(&mut wafer, Arc::new(svc), vec![]).expect("register wafer-run/database");
    let (probe, inits) = Probe::new("test/budget-init", InitBehaviour::WriteRows(rows));
    wafer
        .register_block("test/budget-init", probe)
        .expect("register probe");
    wafer.seal().await.expect("seal");
    (wafer, inits)
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
        matches!(err, InitError::Transient(ref m) if m.contains("config fetch failed")),
        "{err:?}"
    );
    match wafer.init_states().get("test/flaky") {
        Some(BlockInitState::Retrying { reason, retry_in }) => {
            assert!(reason.contains("daily row read limit"), "{reason}");
            assert!(*retry_in > Duration::ZERO && *retry_in <= TRANSIENT_RETRY_BASE);
        }
        other => panic!("expected Retrying inside the backoff, got {other:?}"),
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

/// Two statements with one of the invocation's two left: the database
/// answers `ResourceExhausted` + `STATEMENT_BUDGET_EXHAUSTED`, and the Init
/// that met it fails transiently — a fresh invocation may run it.
#[tokio::test]
async fn an_init_that_meets_a_spent_statement_budget_fails_transiently() {
    let (wafer, inits) = budget_wafer(2, 1, 2).await;
    let err = wafer
        .init_block("test/budget-init")
        .await
        .expect_err("the budget refuses the write");
    match err {
        InitError::Transient(msg) => {
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
}

/// Two statements against a limit of one: no invocation can run it
/// (`StatementLimitExceeded`), so the Init fails permanently.
#[tokio::test]
async fn an_init_whose_write_exceeds_the_whole_statement_limit_fails_permanently() {
    let (wafer, inits) = budget_wafer(1, 0, 2).await;
    let err = wafer
        .init_block("test/budget-init")
        .await
        .expect_err("the limit refuses the write");
    assert!(
        matches!(&err, InitError::Permanent(msg) if msg.contains("InvalidArgument")),
        "the database's own StatementLimitExceeded refusal: {err:?}"
    );
    wafer
        .init_block("test/budget-init")
        .await
        .expect_err("cached");
    assert_eq!(
        inits.load(Ordering::SeqCst),
        1,
        "a permanent failure is cached"
    );
    assert!(matches!(
        wafer.init_states().get("test/budget-init"),
        Some(BlockInitState::Failed { .. })
    ));
}

/// A `ResourceExhausted` without the budget detail code is one of the
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
            UnavailableBlock {
                block: "test/c-transient".to_string(),
                reason: UnavailableReason::Transient,
            },
            UnavailableBlock {
                block: "test/d-fault".to_string(),
                reason: UnavailableReason::SourceFault,
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
        Some(BlockInitState::Retrying { reason, .. }) if reason.contains("config fetch failed")
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
