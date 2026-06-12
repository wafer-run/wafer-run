use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use wafer_block::{
    streams::output::TerminalNotResponse,
    types::{MetaGet, MetaSet},
};
use wafer_run::{ErrorCode, *};

// ---------------------------------------------------------------------------
// Helper: empty Wafer instance
// ---------------------------------------------------------------------------
fn empty_wafer() -> wafer_run::Wafer {
    wafer_run::Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible")
}

// ---------------------------------------------------------------------------
// Helper: build a single-step WaferFlow
// ---------------------------------------------------------------------------
fn single_step_flow(id: &str, block: &str) -> wafer_flow::WaferFlow {
    wafer_flow::WaferFlow {
        id: id.to_string(),
        name: format!("Test flow: {id}"),
        version: "0.0.1".to_string(),
        description: None,
        input: None,
        output: None,
        steps: vec![wafer_flow::Step {
            id: "root".to_string(),
            block: block.to_string(),
            input: None,
            next: None,
            each: None,
            parallel: None,
            description: None,
            config: None,
        }],
        config: None,
        blocks: None,
        config_map: None,
        config_defaults: None,
    }
}

fn step(id: &str, block: &str) -> wafer_flow::Step {
    wafer_flow::Step {
        id: id.to_string(),
        block: block.to_string(),
        input: None,
        next: None,
        each: None,
        parallel: None,
        description: None,
        config: None,
    }
}

fn step_with_config(id: &str, block: &str, config: serde_json::Value) -> wafer_flow::Step {
    wafer_flow::Step {
        id: id.to_string(),
        block: block.to_string(),
        input: None,
        next: None,
        each: None,
        parallel: None,
        description: None,
        config: Some(config),
    }
}

fn make_flow(id: &str, steps: Vec<wafer_flow::Step>) -> wafer_flow::WaferFlow {
    wafer_flow::WaferFlow {
        id: id.to_string(),
        name: format!("Test flow: {id}"),
        version: "0.0.1".to_string(),
        description: None,
        input: None,
        output: None,
        steps,
        config: None,
        blocks: None,
        config_map: None,
        config_defaults: None,
    }
}

fn make_flow_with_on_error(
    id: &str,
    steps: Vec<wafer_flow::Step>,
    on_error: &str,
) -> wafer_flow::WaferFlow {
    wafer_flow::WaferFlow {
        id: id.to_string(),
        name: format!("Test flow: {id}"),
        version: "0.0.1".to_string(),
        description: None,
        input: None,
        output: None,
        steps,
        config: Some(wafer_flow::FlowConfig {
            timeout_ms: None,
            timeout: None,
            max_steps: None,
            on_error: Some(on_error.to_string()),
        }),
        blocks: None,
        config_map: None,
        config_defaults: None,
    }
}

// ---------------------------------------------------------------------------
// Helper: run a flow and collect to a usable test result
// ---------------------------------------------------------------------------

/// Enumeration of the possible flow outcomes in tests.
#[derive(Debug)]
enum TestResult {
    /// The flow completed with a body (OutputStream::respond).
    Respond(Vec<u8>),
    /// The flow completed with a Continue (all middleware passed through; message not captured).
    Continue,
    /// The flow completed with an error.
    Error(WaferError),
    /// The flow completed with a drop.
    Drop,
    /// The flow halted (block short-circuited with a body+meta response).
    #[allow(dead_code)]
    Halt(Vec<u8>),
}

impl TestResult {
    fn is_respond(&self) -> bool {
        matches!(self, Self::Respond(_))
    }
    fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }
    fn is_drop(&self) -> bool {
        matches!(self, Self::Drop)
    }

    fn body(&self) -> &[u8] {
        match self {
            Self::Respond(body) => body,
            _ => panic!("TestResult is not Respond, was: {self:?}"),
        }
    }

    fn error(&self) -> &WaferError {
        match self {
            Self::Error(e) => e,
            _ => panic!("TestResult is not Error, was: {self:?}"),
        }
    }
}

async fn run_flow(w: &Wafer, flow_id: &str, msg: Message, body: Vec<u8>) -> TestResult {
    let input = InputStream::from_bytes(body);
    let output = w.run(flow_id, msg, input).await;
    match output.collect_buffered().await {
        Ok(buf) => TestResult::Respond(buf.body),
        Err(TerminalNotResponse::Continue(_)) => TestResult::Continue,
        Err(TerminalNotResponse::Error(e)) => TestResult::Error(e),
        Err(TerminalNotResponse::Drop) => TestResult::Drop,
        Err(TerminalNotResponse::Halt(buf)) => TestResult::Halt(buf.body),
        Err(TerminalNotResponse::Malformed) => panic!("malformed output stream"),
    }
}

// ===========================================================================
// 1. Basic runtime creation and block registration
// ===========================================================================

#[test]
fn test_create_runtime() {
    let w = empty_wafer();
    assert!(w.flows_info().is_empty());
}

struct EchoBlock;

#[async_trait::async_trait]
impl Block for EchoBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/echo", "0.0.1", "http-handler@v1", "Echo")
            .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        let body = input.collect_to_bytes().await;
        OutputStream::respond(body) // pass body through
    }
}

#[test]
fn test_register_inline_block() {
    let mut w = empty_wafer();
    w.register_block("test/echo", Arc::new(EchoBlock)).unwrap();
    assert!(w.has_block("test/echo"));
    assert!(!w.has_block("nonexistent"));
}

// ===========================================================================
// 2. Build and execute a single-block flow
// ===========================================================================

struct UpperBlock;

#[async_trait::async_trait]
impl Block for UpperBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/upper", "0.0.1", "http-handler@v1", "Upper")
            .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        let bytes = input.collect_to_bytes().await;
        let text = String::from_utf8_lossy(&bytes).to_uppercase();
        OutputStream::respond(text.into_bytes())
    }
}

#[tokio::test]
async fn test_single_block_flow() {
    let mut w = empty_wafer();
    w.register_block("test/upper", Arc::new(UpperBlock))
        .unwrap();
    w.add_flow(single_step_flow("to-upper", "test/upper"));
    w.seal().await.expect("seal failed");

    let result = run_flow(
        &w,
        "to-upper",
        Message::new("text"),
        b"hello world".to_vec(),
    )
    .await;
    assert!(result.is_respond(), "expected respond, got: {result:?}");
    assert_eq!(String::from_utf8_lossy(result.body()), "HELLO WORLD");
}

// ===========================================================================
// 3. Multi-block sequential flow (a -> b -> c) — middleware mode
// ===========================================================================

struct AppendBlock(String);

#[async_trait::async_trait]
impl Block for AppendBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/append", "0.0.1", "http-handler@v1", "Append")
            .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        let bytes = input.collect_to_bytes().await;
        let mut text = String::from_utf8_lossy(&bytes).to_string();
        text.push_str(&self.0);
        OutputStream::respond(text.into_bytes())
    }
}

#[tokio::test]
async fn test_sequential_flow() {
    let mut w = empty_wafer();

    // Block A: append "-A"
    w.register_block("test/append-a", Arc::new(AppendBlock("-A".to_string())))
        .unwrap();
    // Block B: append "-B"
    w.register_block("test/append-b", Arc::new(AppendBlock("-B".to_string())))
        .unwrap();
    // Block C: append "-C"
    w.register_block("test/append-c", Arc::new(AppendBlock("-C".to_string())))
        .unwrap();

    // Build flow: A -> B -> C (sequential steps)
    w.add_flow(make_flow(
        "abc",
        vec![
            step("a", "test/append-a"),
            step("b", "test/append-b"),
            step("c", "test/append-c"),
        ],
    ));
    w.seal().await.expect("seal failed");

    let result = run_flow(&w, "abc", Message::new("test"), b"start".to_vec()).await;
    assert!(result.is_respond(), "expected respond, got: {result:?}");
    assert_eq!(String::from_utf8_lossy(result.body()), "start-A-B-C");
}

// ===========================================================================
// 4. Pattern matching — using router block directly
// ===========================================================================

#[test]
fn test_pattern_matching_http_style() {
    assert!(matches_pattern("GET:/users", "GET:/users"));
    assert!(matches_pattern("*:/users", "POST:/users"));
    assert!(!matches_pattern("GET:/users", "POST:/users"));
    assert!(matches_pattern("GET:/users/{id}", "GET:/users/42"));
    assert!(matches_pattern("GET:/users/**", "GET:/users/42/profile"));
}

// ===========================================================================
// 5. Router
// ===========================================================================

#[tokio::test]
async fn test_router_basic() {
    let mut router = Router::new();

    router.retrieve("/items", |_ctx, _msg: Message, _input: InputStream| {
        let body = serde_json::to_vec(&serde_json::json!({"items": []})).unwrap();
        OutputStream::respond(body)
    });

    router.create("/items", |_ctx, _msg: Message, _input: InputStream| {
        let body = serde_json::to_vec(&serde_json::json!({"id": "new-1"})).unwrap();
        OutputStream::respond(body)
    });

    router.retrieve("/items/{id}", |_ctx, msg: Message, _input: InputStream| {
        let id = msg.var("id").to_string();
        let body = serde_json::to_vec(&serde_json::json!({"id": id})).unwrap();
        OutputStream::respond(body)
    });

    // Simulate a GET /items request
    let ctx = make_test_context();
    let mut msg = Message::new("http.request");
    msg.set_meta("req.action", "retrieve");
    msg.set_meta("req.resource", "/items");

    let result = router
        .route(&ctx, msg, InputStream::empty())
        .collect_buffered()
        .await;
    let body: serde_json::Value = serde_json::from_slice(&result.unwrap().body).unwrap();
    assert_eq!(body["items"], serde_json::json!([]));

    // Simulate POST /items
    let mut msg = Message::new("http.request");
    msg.set_meta("req.action", "create");
    msg.set_meta("req.resource", "/items");

    let result = router
        .route(&ctx, msg, InputStream::empty())
        .collect_buffered()
        .await;
    let body: serde_json::Value = serde_json::from_slice(&result.unwrap().body).unwrap();
    assert_eq!(body["id"], "new-1");

    // Simulate GET /items/42
    let mut msg = Message::new("http.request");
    msg.set_meta("req.action", "retrieve");
    msg.set_meta("req.resource", "/items/42");

    let result = router
        .route(&ctx, msg, InputStream::empty())
        .collect_buffered()
        .await;
    let body: serde_json::Value = serde_json::from_slice(&result.unwrap().body).unwrap();
    assert_eq!(body["id"], "42");
}

#[tokio::test]
async fn test_router_not_found() {
    let router = Router::new();
    let ctx = make_test_context();

    let mut msg = Message::new("http.request");
    msg.set_meta("req.action", "retrieve");
    msg.set_meta("req.resource", "/nonexistent");

    let result = router
        .route(&ctx, msg, InputStream::empty())
        .collect_buffered()
        .await;
    match result {
        Err(TerminalNotResponse::Error(e)) => assert_eq!(e.code, ErrorCode::NotFound),
        other => panic!("expected error, got {other:?}"),
    }
}

#[tokio::test]
async fn test_router_update_delete() {
    let mut router = Router::new();

    router.update("/items/{id}", |_ctx, msg: Message, _input: InputStream| {
        let id = msg.var("id").to_string();
        let body = serde_json::to_vec(&serde_json::json!({"updated": id})).unwrap();
        OutputStream::respond(body)
    });

    router.delete("/items/{id}", |_ctx, _msg: Message, _input: InputStream| {
        OutputStream::drop_request()
    });

    let ctx = make_test_context();

    // Update
    let mut msg = Message::new("http.request");
    msg.set_meta("req.action", "update");
    msg.set_meta("req.resource", "/items/99");

    let result = router
        .route(&ctx, msg, InputStream::empty())
        .collect_buffered()
        .await;
    let body: serde_json::Value = serde_json::from_slice(&result.unwrap().body).unwrap();
    assert_eq!(body["updated"], "99");

    // Delete
    let mut msg = Message::new("http.request");
    msg.set_meta("req.action", "delete");
    msg.set_meta("req.resource", "/items/99");

    let result = router
        .route(&ctx, msg, InputStream::empty())
        .collect_buffered()
        .await;
    assert!(matches!(result, Err(TerminalNotResponse::Drop)));
}

// ===========================================================================
// 6. OutputStream helpers
// ===========================================================================

#[tokio::test]
async fn test_respond_helper() {
    let output = OutputStream::respond(b"ok".to_vec());
    let buf = output.collect_buffered().await.unwrap();
    assert_eq!(buf.body, b"ok");
}

#[tokio::test]
async fn test_error_helper() {
    let output = OutputStream::error(WaferError {
        code: ErrorCode::InvalidArgument,
        message: "missing field".to_string(),
        meta: vec![],
    });
    match output.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => {
            assert_eq!(e.code, ErrorCode::InvalidArgument);
            assert_eq!(e.message, "missing field");
        }
        other => panic!("expected error, got {other:?}"),
    }
}

#[tokio::test]
async fn test_drop_helper() {
    let output = OutputStream::drop_request();
    assert!(matches!(
        output.collect_buffered().await,
        Err(TerminalNotResponse::Drop)
    ));
}

#[tokio::test]
async fn test_continue_helper() {
    let mut msg = Message::new("test");
    msg.set_meta("key", "value");
    let output = OutputStream::continue_with(msg.clone());
    match output.collect_buffered().await {
        Err(TerminalNotResponse::Continue(m)) => {
            assert_eq!(m.get_meta("key"), "value");
        }
        other => panic!("expected continue, got {other:?}"),
    }
}

// ===========================================================================
// 6b. `requires` enforcement through top-level dispatch
// ===========================================================================

// A block that declares a `requires` list and, when handled, tries to
// `call_block` a target that is NOT in that list — which must be denied.
struct RequiresCallerBlock;

#[async_trait::async_trait]
impl Block for RequiresCallerBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/requires-caller", "0.0.1", "http-handler@v1", "Caller")
            .instance_mode(InstanceMode::Singleton)
            .requires(vec!["test/allowed".to_string()])
    }
    async fn handle(&self, ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        // "test/echo" is not in the requires list, so this call must be denied.
        ctx.call_block("test/echo", Message::new("test.call"), InputStream::empty())
            .await
    }
}

// Regression for the dispatch perf refactor that reads `requires` from the
// startup snapshot instead of rebuilding `BlockInfo` on every `run_block`:
// a declared `requires` list must still be populated and enforced.
#[tokio::test]
async fn run_block_enforces_requires_read_from_snapshot() {
    let mut w = empty_wafer();
    w.register_block("test/requires-caller", Arc::new(RequiresCallerBlock))
        .unwrap();
    w.register_block("test/echo", Arc::new(EchoBlock)).unwrap();
    w.seal().await.expect("seal");

    let out = Arc::new(w)
        .run_block(
            "test/requires-caller",
            Message::new("test.req"),
            InputStream::empty(),
        )
        .await;

    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => {
            assert_eq!(
                e.code,
                ErrorCode::PermissionDenied,
                "call_block to a block outside the declared requires list must be denied"
            );
        }
        other => panic!("expected PERMISSION_DENIED, got {other:?}"),
    }
}

// ===========================================================================
// 7. Observability hooks
// ===========================================================================

struct NoopBlock;

#[async_trait::async_trait]
impl Block for NoopBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/noop", "0.0.1", "http-handler@v1", "Noop")
            .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        let body = input.collect_to_bytes().await;
        OutputStream::respond(body)
    }
}

#[tokio::test]
async fn test_observability_flow_hooks() {
    let mut w = empty_wafer();

    let flow_start_count = Arc::new(AtomicUsize::new(0));
    let flow_end_count = Arc::new(AtomicUsize::new(0));
    let cs = flow_start_count.clone();
    let ce = flow_end_count.clone();

    w.hooks.on_flow_start(move |_flow_id, _msg| {
        cs.fetch_add(1, Ordering::SeqCst);
    });
    w.hooks.on_flow_end(move |_flow_id, _dur| {
        ce.fetch_add(1, Ordering::SeqCst);
    });

    w.register_block("test/noop", Arc::new(NoopBlock)).unwrap();

    w.add_flow(single_step_flow("observed", "test/noop"));
    w.seal().await.expect("seal failed");

    run_flow(&w, "observed", Message::new("test"), b"data".to_vec()).await;

    assert_eq!(flow_start_count.load(Ordering::SeqCst), 1);
    assert_eq!(flow_end_count.load(Ordering::SeqCst), 1);

    // Execute again
    run_flow(&w, "observed", Message::new("test"), b"data".to_vec()).await;
    assert_eq!(flow_start_count.load(Ordering::SeqCst), 2);
    assert_eq!(flow_end_count.load(Ordering::SeqCst), 2);
}

struct Step1Block;
struct Step2Block;

#[async_trait::async_trait]
impl Block for Step1Block {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/step-1", "0.0.1", "http-handler@v1", "Step 1")
            .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        OutputStream::respond(input.collect_to_bytes().await)
    }
}

#[async_trait::async_trait]
impl Block for Step2Block {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/step-2", "0.0.1", "http-handler@v1", "Step 2")
            .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        OutputStream::respond(input.collect_to_bytes().await)
    }
}

#[tokio::test]
async fn test_observability_block_hooks() {
    let mut w = empty_wafer();

    let block_names = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let bn = block_names.clone();

    w.hooks.on_block_start(move |ctx| {
        bn.lock().push(ctx.block_name.clone());
    });

    let block_durations = Arc::new(parking_lot::Mutex::new(Vec::<Duration>::new()));
    let bd = block_durations.clone();

    w.hooks.on_block_end(move |_ctx, duration| {
        bd.lock().push(duration);
    });

    w.register_block("test/step-1", Arc::new(Step1Block))
        .unwrap();
    w.register_block("test/step-2", Arc::new(Step2Block))
        .unwrap();

    w.add_flow(make_flow(
        "two-steps",
        vec![step("s1", "test/step-1"), step("s2", "test/step-2")],
    ));
    w.seal().await.expect("seal failed");

    run_flow(&w, "two-steps", Message::new("test"), vec![]).await;

    let names = block_names.lock().clone();
    assert_eq!(names.len(), 2);
    assert_eq!(names[0], "test/step-1");
    assert_eq!(names[1], "test/step-2");

    let durations_len = block_durations.lock().len();
    assert_eq!(durations_len, 2);
}

// A block that fans out to `test/noop` via `call_block` — used to verify the
// nested block-to-block dispatch path fires the same observability hooks as
// top-level dispatch (it previously fired none).
struct NestedCallerBlock;

#[async_trait::async_trait]
impl Block for NestedCallerBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/nested-caller",
            "0.0.1",
            "http-handler@v1",
            "Nested caller",
        )
        .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        // "retrieve" is a valid http-handler@v1 action, so the interface
        // validator lets the nested dispatch through.
        ctx.call_block("test/noop", Message::new("retrieve"), input)
            .await
    }
}

#[tokio::test]
async fn test_observability_block_hooks_fire_on_nested_call_block() {
    let mut w = empty_wafer();

    // (block_name, node_path) per block_start firing, in order.
    let started = Arc::new(parking_lot::Mutex::new(Vec::<(String, String)>::new()));
    let s = started.clone();
    w.hooks.on_block_start(move |ctx| {
        s.lock()
            .push((ctx.block_name.clone(), ctx.node_path.clone()));
    });

    let end_count = Arc::new(AtomicUsize::new(0));
    let e = end_count.clone();
    w.hooks.on_block_end(move |_ctx, _dur| {
        e.fetch_add(1, Ordering::SeqCst);
    });

    w.register_block("test/nested-caller", Arc::new(NestedCallerBlock))
        .unwrap();
    w.register_block("test/noop", Arc::new(NoopBlock)).unwrap();
    w.seal().await.expect("seal failed");

    let out = w
        .run_block(
            "test/nested-caller",
            Message::new("test"),
            InputStream::empty(),
        )
        .await;
    out.collect_buffered()
        .await
        .expect("nested dispatch succeeds");

    let frames = started.lock().clone();
    let names: Vec<&str> = frames.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        vec!["test/nested-caller", "test/noop"],
        "nested call_block must fire block_start for the callee, after the caller's"
    );
    assert_eq!(
        frames[1].1, "test/noop",
        "nested span's node_path must be the resolved callee name"
    );
    assert_eq!(
        end_count.load(Ordering::SeqCst),
        2,
        "block_end must fire for both the outer and the nested frame"
    );
}

// ===========================================================================
// 8. Flow references via next routing with flow field
// ===========================================================================

struct ValidateBlock;
struct StoreBlock;

#[async_trait::async_trait]
impl Block for ValidateBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/validate", "0.0.1", "http-handler@v1", "Validate")
            .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(
        &self,
        _ctx: &dyn Context,
        mut msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        msg.set_meta("validated", "true");
        OutputStream::continue_with(msg)
    }
}

#[async_trait::async_trait]
impl Block for StoreBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/store", "0.0.1", "http-handler@v1", "Store")
            .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(b"stored".to_vec())
    }
}

#[tokio::test]
async fn test_flow_reference() {
    let mut w = empty_wafer();

    w.register_block("test/validate", Arc::new(ValidateBlock))
        .unwrap();
    w.register_block("test/store", Arc::new(StoreBlock))
        .unwrap();

    // Inner flow: validate
    w.add_flow(single_step_flow("validation-flow", "test/validate"));

    // Outer flow: validate then store
    w.add_flow(make_flow(
        "main-flow",
        vec![step("v", "test/validate"), step("s", "test/store")],
    ));
    w.seal().await.expect("seal failed");

    let result = run_flow(
        &w,
        "main-flow",
        Message::new("user.create"),
        b"data".to_vec(),
    )
    .await;
    assert!(result.is_respond(), "expected respond, got: {result:?}");
    assert_eq!(result.body(), b"stored");
}

struct ShouldNotRunBlock;

#[async_trait::async_trait]
impl Block for ShouldNotRunBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/should-not-run",
            "0.0.1",
            "http-handler@v1",
            "Should Not Run",
        )
        .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(b"this should never appear".to_vec())
    }
}

// In the new streaming model, ALL Ok(respond) results continue to the next step.
// Drop is the only way to stop processing without an error.
#[tokio::test]
async fn test_drop_short_circuits_flow() {
    let mut w = empty_wafer();

    w.register_block("test/dropper-2", Arc::new(DropperBlock))
        .unwrap();
    w.register_block("test/should-not-run-2", Arc::new(ShouldNotRunBlock))
        .unwrap();

    w.add_flow(make_flow(
        "drop-short-circuit",
        vec![
            step("d", "test/dropper-2"),
            step("s", "test/should-not-run-2"),
        ],
    ));
    w.seal().await.expect("seal failed");

    // Drop in step 1 causes the entire flow to stop with Drop result
    let result = run_flow(&w, "drop-short-circuit", Message::new("test"), vec![]).await;
    assert!(
        result.is_drop(),
        "expected drop to short-circuit flow, got: {result:?}"
    );
}

#[tokio::test]
async fn test_flow_reference_not_found() {
    let mut w = empty_wafer();

    w.register_block("test/noop-2", Arc::new(NoopBlock))
        .unwrap();

    // Create a flow with next routing to non-existent flow
    let mut s = step("root", "test/noop-2");
    s.next = Some(vec![wafer_flow::NextEntry {
        when: None,
        step: None,
        flow: Some("does-not-exist".to_string()),
    }]);

    w.add_flow(make_flow("bad-ref", vec![s]));
    w.seal().await.expect("seal failed");

    let result = run_flow(&w, "bad-ref", Message::new("test"), vec![]).await;
    assert!(result.is_error(), "expected error, got: {result:?}");
    assert!(
        result.error().message.contains("does-not-exist"),
        "error: {:?}",
        result.error().message
    );
}

// ===========================================================================
// 9. Error handling: on_error = stop vs continue
// ===========================================================================

struct FailBlock;
struct AfterFailBlock;

#[async_trait::async_trait]
impl Block for FailBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/fail", "0.0.1", "http-handler@v1", "Fail")
            .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::error(WaferError::new(ErrorCode::Unknown, "intentional failure"))
    }
}

#[async_trait::async_trait]
impl Block for AfterFailBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/after-fail", "0.0.1", "http-handler@v1", "After Fail")
            .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(b"recovered".to_vec())
    }
}

#[tokio::test]
async fn test_on_error_stop() {
    let mut w = empty_wafer();

    w.register_block("test/fail", Arc::new(FailBlock)).unwrap();
    w.register_block("test/after-fail", Arc::new(AfterFailBlock))
        .unwrap();

    w.add_flow(make_flow_with_on_error(
        "stop-flow",
        vec![step("f", "test/fail"), step("a", "test/after-fail")],
        "stop",
    ));
    w.seal().await.expect("seal failed");

    let result = run_flow(&w, "stop-flow", Message::new("test"), vec![]).await;
    assert!(result.is_error(), "expected error, got: {result:?}");
    // "test_error" is not a known ErrorCode variant, maps to Unknown
    assert_eq!(result.error().code, ErrorCode::Unknown);
}

#[tokio::test]
async fn test_on_error_continue() {
    let mut w = empty_wafer();

    let fail_count = Arc::new(AtomicUsize::new(0));
    let fc = fail_count.clone();

    struct CountingFailBlock(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl Block for CountingFailBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new(
                "test/fail-counting",
                "0.0.1",
                "http-handler@v1",
                "CountingFail",
            )
            .instance_mode(InstanceMode::Singleton)
        }
        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            self.0.fetch_add(1, Ordering::SeqCst);
            OutputStream::error(WaferError::new(ErrorCode::Unknown, "intentional failure"))
        }
    }

    w.register_block("test/fail-counting", Arc::new(CountingFailBlock(fc)))
        .unwrap();
    w.register_block("test/after-fail-2", Arc::new(AfterFailBlock))
        .unwrap();

    w.add_flow(make_flow_with_on_error(
        "cont-flow",
        vec![
            step("f", "test/fail-counting"),
            step("a", "test/after-fail-2"),
        ],
        "continue",
    ));
    w.seal().await.expect("seal failed");

    let result = run_flow(&w, "cont-flow", Message::new("test"), vec![]).await;

    // With on_error=continue, the flow proceeds past the error
    assert_eq!(fail_count.load(Ordering::SeqCst), 1);
    assert!(result.is_respond(), "expected respond, got: {result:?}");
    assert_eq!(result.body(), b"recovered");
}

#[tokio::test]
async fn test_on_error_continue_no_more_nodes() {
    let mut w = empty_wafer();

    struct FailAtEndBlock;
    #[async_trait::async_trait]
    impl Block for FailAtEndBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/fail-at-end", "0.0.1", "http-handler@v1", "FailAtEnd")
                .instance_mode(InstanceMode::Singleton)
        }
        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            OutputStream::error(WaferError::new(ErrorCode::Unknown, "error at tail"))
        }
    }

    w.register_block("test/fail-at-end", Arc::new(FailAtEndBlock))
        .unwrap();

    w.add_flow(make_flow_with_on_error(
        "cont-end",
        vec![step("f", "test/fail-at-end")],
        "continue",
    ));
    w.seal().await.expect("seal failed");

    let result = run_flow(&w, "cont-end", Message::new("test"), vec![]).await;

    // on_error=continue with no more steps means the flow continues with empty body
    assert!(result.is_respond(), "expected respond, got: {result:?}");
}

// ===========================================================================
// 10. Drop action
// ===========================================================================

struct DropperBlock;

#[async_trait::async_trait]
impl Block for DropperBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/dropper", "0.0.1", "http-handler@v1", "Dropper")
            .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::drop_request()
    }
}

#[tokio::test]
async fn test_drop_action() {
    let mut w = empty_wafer();

    w.register_block("test/dropper", Arc::new(DropperBlock))
        .unwrap();
    w.register_block("test/unreachable-after-drop", Arc::new(ShouldNotRunBlock))
        .unwrap();

    w.add_flow(make_flow(
        "drop-flow",
        vec![
            step("d", "test/dropper"),
            step("u", "test/unreachable-after-drop"),
        ],
    ));
    w.seal().await.expect("seal failed");

    let result = run_flow(&w, "drop-flow", Message::new("test"), b"data".to_vec()).await;
    assert!(result.is_drop(), "expected drop, got: {result:?}");
}

// ===========================================================================
// 12. Flow not found
// ===========================================================================

#[tokio::test]
async fn test_execute_nonexistent_flow() {
    let w = empty_wafer();

    let result = run_flow(&w, "nonexistent", Message::new("test"), b"data".to_vec()).await;
    assert!(result.is_error(), "expected error, got: {result:?}");
    // "flow not found" maps to NotFound
    assert_eq!(result.error().code, ErrorCode::NotFound);
}

// ===========================================================================
// 13. Block with config
// ===========================================================================

struct ConfigurableBlock;

#[async_trait::async_trait]
impl Block for ConfigurableBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/configurable",
            "0.0.1",
            "http-handler@v1",
            "Configurable",
        )
        .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        let prefix = ctx.config_get("prefix").unwrap_or("default").to_string();
        let bytes = input.collect_to_bytes().await;
        let text = format!("{}-{}", prefix, String::from_utf8_lossy(&bytes));
        OutputStream::respond(text.into_bytes())
    }
}

#[tokio::test]
async fn test_block_with_config() {
    let mut w = empty_wafer();

    w.register_block("test/configurable", Arc::new(ConfigurableBlock))
        .unwrap();

    w.add_flow(make_flow(
        "config-flow",
        vec![step_with_config(
            "root",
            "test/configurable",
            serde_json::json!({"prefix": "hello"}),
        )],
    ));
    w.seal().await.expect("seal failed");

    let result = run_flow(&w, "config-flow", Message::new("test"), b"world".to_vec()).await;
    assert!(result.is_respond(), "expected respond, got: {result:?}");
    assert_eq!(String::from_utf8_lossy(result.body()), "hello-world");
}

// ===========================================================================
// 14. Message methods
// ===========================================================================

#[test]
fn test_message_methods() {
    let mut msg = Message::new("user.create");
    msg.set_meta("req.action", "create");
    msg.set_meta("req.resource", "/users");
    msg.set_meta("req.param.id", "42");
    msg.set_meta("req.query.page", "2");
    msg.set_meta("req.content_type", "application/json");
    msg.set_meta("auth.user_id", "user-1");
    msg.set_meta("auth.user_email", "alice@example.com");
    msg.set_meta("auth.user_roles", "admin,editor");
    msg.set_meta("req.client.ip", "127.0.0.1");
    msg.set_meta("http.header.cookie", "session=abc; theme=dark");

    assert_eq!(msg.action(), "create");
    assert_eq!(msg.path(), "/users");
    assert_eq!(msg.var("id"), "42");
    assert_eq!(msg.query("page"), "2");
    assert_eq!(msg.get_meta("req.content_type"), "application/json");
    assert_eq!(msg.user_id(), "user-1");
    assert_eq!(msg.get_meta("auth.user_email"), "alice@example.com");
    let roles: Vec<&str> = msg.get_meta("auth.user_roles").split(',').collect();
    assert_eq!(roles, vec!["admin", "editor"]);
    assert!(roles.contains(&"admin"));
    assert_eq!(msg.remote_addr(), "127.0.0.1");
    assert_eq!(msg.cookie("session"), "abc");
    assert_eq!(msg.cookie("theme"), "dark");
    assert_eq!(msg.cookie("nonexistent"), "");

    let params = msg.query_params();
    assert_eq!(*params.get("page").unwrap(), "2");

    let (page, page_size, offset) = msg.pagination_params(20);
    assert_eq!(page, 2);
    assert_eq!(page_size, 20);
    assert_eq!(offset, 20);
}

// ===========================================================================
// 15. Resolve errors
// ===========================================================================

#[tokio::test]
async fn test_resolve_missing_block() {
    let mut w = empty_wafer();

    w.add_flow(single_step_flow("broken", "unregistered-block"));

    let err = w.seal().await.unwrap_err().to_string();
    assert!(err.contains("unregistered-block"), "Error: {err}");
}

// ===========================================================================
// 16. WaferFlow JSON-based definition
// ===========================================================================

#[tokio::test]
async fn test_add_flow_json() {
    let mut w = empty_wafer();

    w.register_block("test/echo-2", Arc::new(EchoBlock))
        .unwrap();

    w.add_flow_json(
        r#"{
        "id": "from-json",
        "name": "From JSON",
        "version": "0.1.0",
        "steps": [{ "id": "root", "block": "test/echo-2" }],
        "config": { "on_error": "stop", "timeout": "30s" }
    }"#,
    )
    .expect("add_flow_json failed");
    w.seal().await.expect("seal failed");

    let result = run_flow(&w, "from-json", Message::new("test"), b"hello".to_vec()).await;
    // EchoBlock passes body through, so responds with "hello"
    assert!(result.is_respond(), "expected respond, got: {result:?}");
    assert_eq!(result.body(), b"hello");
}

// ===========================================================================
// 17. Panic recovery
// ===========================================================================

struct PanickerBlock;

#[async_trait::async_trait]
impl Block for PanickerBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/panicker", "0.0.1", "http-handler@v1", "Panicker")
            .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        panic!("block went wrong");
    }
}

#[tokio::test]
async fn test_panic_recovery() {
    let mut w = empty_wafer();

    w.register_block("test/panicker", Arc::new(PanickerBlock))
        .unwrap();

    w.add_flow(single_step_flow("panic-flow", "test/panicker"));
    w.seal().await.expect("seal failed");

    let result = run_flow(&w, "panic-flow", Message::new("test"), vec![]).await;
    assert!(result.is_error(), "expected error, got: {result:?}");
    let err = result.error();
    // "panic" maps to Internal (block panicked)
    assert_eq!(err.code, ErrorCode::Internal);
    assert!(err.message.contains("block went wrong"));
}

// ===========================================================================
// 18. Multiple flows info
// ===========================================================================

#[test]
fn test_flows_info() {
    let mut w = empty_wafer();

    w.register_block("test/noop-3", Arc::new(NoopBlock))
        .unwrap();

    w.add_flow(single_step_flow("flow-a", "test/noop-3"));
    w.add_flow(single_step_flow("flow-b", "test/noop-3"));

    let info = w.flows_info();
    assert_eq!(info.len(), 2);

    let ids: Vec<&str> = info.iter().map(|c| c.id.as_str()).collect();
    assert!(ids.contains(&"flow-a"));
    assert!(ids.contains(&"flow-b"));
}

// ===========================================================================
// 19. Start / Stop lifecycle
// ===========================================================================

struct LifecycleBlock;

#[async_trait::async_trait]
impl Block for LifecycleBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/lifecycle-block",
            "0.0.1",
            "http-handler@v1",
            "Lifecycle block",
        )
        .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        OutputStream::respond(input.collect_to_bytes().await)
    }
}

#[tokio::test]
async fn test_start_and_stop() {
    let mut w = empty_wafer();

    w.register_block("test/lifecycle-block", Arc::new(LifecycleBlock))
        .unwrap();

    w.add_flow(single_step_flow("lifecycle-test", "test/lifecycle-block"));

    // Start implicitly resolves if not already resolved
    w.seal().await.expect("start failed");

    let result = run_flow(&w, "lifecycle-test", Message::new("test"), vec![]).await;
    assert!(result.is_respond(), "expected respond, got: {result:?}");

    // Shutdown calls lifecycle(Stop) on all resolved blocks
    w.shutdown().await;
}

// ===========================================================================
// 20. WaferError
// ===========================================================================

#[test]
fn test_wafer_error_meta() {
    let mut err = WaferError::new(ErrorCode::NotFound, "user not found");
    err.meta.set("resource".to_string(), "users".to_string());
    err.meta.set("id".to_string(), "42".to_string());

    assert_eq!(err.code, ErrorCode::NotFound);
    assert_eq!(err.message, "user not found");
    assert_eq!(err.meta.get("resource").unwrap(), "users");
    assert_eq!(err.meta.get("id").unwrap(), "42");
}

// ===========================================================================
// Test context helper (minimal Context implementation for router tests)
// ===========================================================================

#[derive(Clone)]
struct TestContext;

#[async_trait::async_trait]
impl Context for TestContext {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        input: InputStream,
    ) -> OutputStream {
        // Simple mock: echo back body
        let body = input.collect_to_bytes().await;
        OutputStream::respond(body)
    }

    fn is_cancelled(&self) -> bool {
        false
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }

    fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
        std::sync::Arc::new(self.clone())
    }
}

fn make_test_context() -> TestContext {
    TestContext
}

// ===========================================================================
// 22. parse_versioned_block tests
// ===========================================================================

#[cfg(feature = "wasm")]
mod versioned_block_tests {
    use wafer_run::parse_versioned_block;

    // --- org/block@version format ---

    #[test]
    fn test_parse_versioned_basic() {
        let r = parse_versioned_block("acme/auth-block@v1.0.0").unwrap();
        assert_eq!(r.org, "acme");
        assert_eq!(r.block, "auth-block");
        assert_eq!(r.version, "v1.0.0");
    }

    #[test]
    fn test_parse_versioned_wafer_run() {
        let r = parse_versioned_block("wafer-run/auth@v1.0.0").unwrap();
        assert_eq!(r.org, "wafer-run");
        assert_eq!(r.block, "auth");
        assert_eq!(r.version, "v1.0.0");
    }

    #[test]
    fn test_parse_versioned_no_version() {
        assert!(parse_versioned_block("wafer-run/sqlite").is_none());
    }

    #[test]
    fn test_parse_versioned_latest_rejected() {
        assert!(parse_versioned_block("acme/auth-block@latest").is_none());
    }

    #[test]
    fn test_parse_versioned_empty_version() {
        assert!(parse_versioned_block("acme/auth-block@").is_none());
    }

    #[test]
    fn test_parse_versioned_prerelease() {
        let r = parse_versioned_block("acme/auth-block@v2.0.0-rc.1").unwrap();
        assert_eq!(r.org, "acme");
        assert_eq!(r.block, "auth-block");
        assert_eq!(r.version, "v2.0.0-rc.1");
    }

    #[test]
    fn test_parse_versioned_wrong_segments() {
        assert!(parse_versioned_block("acme@v1.0.0").is_none());
        assert!(parse_versioned_block("acme/auth/extra@v1.0.0").is_none());
    }

    #[test]
    fn test_parse_versioned_plain_name_rejected() {
        assert!(parse_versioned_block("plain-name").is_none());
    }
}

// ===========================================================================
// 22b. parse_unversioned_block tests
// ===========================================================================

#[cfg(feature = "wasm")]
mod unversioned_block_tests {
    use wafer_run::parse_unversioned_block;

    #[test]
    fn test_parse_unversioned_basic() {
        let r = parse_unversioned_block("acme/auth-block").unwrap();
        assert_eq!(r.org, "acme");
        assert_eq!(r.block, "auth-block");
        assert_eq!(r.version, "latest");
    }

    #[test]
    fn test_parse_unversioned_wafer_run() {
        let r = parse_unversioned_block("wafer-run/sqlite").unwrap();
        assert_eq!(r.org, "wafer-run");
        assert_eq!(r.block, "sqlite");
        assert_eq!(r.version, "latest");
    }

    #[test]
    fn test_parse_unversioned_at_latest() {
        let r = parse_unversioned_block("acme/auth-block@latest").unwrap();
        assert_eq!(r.org, "acme");
        assert_eq!(r.block, "auth-block");
        assert_eq!(r.version, "latest");
    }

    #[test]
    fn test_parse_unversioned_wafer_run_at_latest() {
        let r = parse_unversioned_block("wafer-run/sqlite@latest").unwrap();
        assert_eq!(r.org, "wafer-run");
        assert_eq!(r.block, "sqlite");
        assert_eq!(r.version, "latest");
    }

    #[test]
    fn test_parse_unversioned_with_version_rejected() {
        assert!(parse_unversioned_block("acme/auth-block@v1.0.0").is_none());
    }

    #[test]
    fn test_parse_unversioned_wrong_segments() {
        assert!(parse_unversioned_block("acme").is_none());
        assert!(parse_unversioned_block("acme/repo/block").is_none());
    }

    #[test]
    fn test_parse_unversioned_empty_segments() {
        assert!(parse_unversioned_block("/auth-block").is_none());
        assert!(parse_unversioned_block("acme/").is_none());
    }

    #[test]
    fn test_parse_unversioned_plain_name_rejected() {
        assert!(parse_unversioned_block("my-block").is_none());
    }
}

// ===========================================================================
// 23. Remote block resolve error paths
// ===========================================================================

#[cfg(feature = "wasm")]
#[tokio::test]
async fn test_resolve_versioned_block_download_error() {
    let mut w = empty_wafer();

    // Use a nonexistent remote block in a flow to trigger resolve error
    w.add_flow(single_step_flow(
        "remote-test",
        "acme/nonexistent-block@v1.0.0",
    ));

    let err = w.seal().await.unwrap_err().to_string();
    assert!(
        err.contains("not found")
            || err.contains("failed to download")
            || err.contains("failed to load remote block")
            || err.contains("failed to fetch registry"),
        "Expected block-not-found or download error, got: {err}"
    );
}

#[cfg(feature = "wasm")]
#[tokio::test]
async fn test_resolve_unversioned_block_download_error() {
    let mut w = empty_wafer();

    w.add_flow(single_step_flow(
        "unversioned-test",
        "acme/nonexistent-block",
    ));

    let err = w.seal().await.unwrap_err().to_string();
    assert!(
        err.contains("not found")
            || err.contains("failed to fetch releases")
            || err.contains("failed to download")
            || err.contains("failed to fetch registry")
            || err.contains("HTTP"),
        "Expected block-not-found or download error, got: {err}"
    );
}

// ===========================================================================
// WaferFlow integration tests — data pipeline mode
// ===========================================================================

#[tokio::test]
async fn test_waferflow_simple_pipeline() {
    let mut w = empty_wafer();

    // Block that uppercases the "text" field
    struct UppercaseBlock;
    #[async_trait::async_trait]
    impl Block for UppercaseBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/uppercase", "0.0.1", "http-handler@v1", "Uppercase")
                .instance_mode(InstanceMode::Singleton)
        }
        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            input: InputStream,
        ) -> OutputStream {
            let bytes = input.collect_to_bytes().await;
            let input_val: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
            let text = input_val.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let output =
                serde_json::to_vec(&serde_json::json!({ "text": text.to_uppercase() })).unwrap();
            OutputStream::respond(output)
        }
    }

    // Block that wraps text in a greeting
    struct GreetBlock;
    #[async_trait::async_trait]
    impl Block for GreetBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/greet", "0.0.1", "http-handler@v1", "Greet")
                .instance_mode(InstanceMode::Singleton)
        }
        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            input: InputStream,
        ) -> OutputStream {
            let bytes = input.collect_to_bytes().await;
            let input_val: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
            let text = input_val.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let output =
                serde_json::to_vec(&serde_json::json!({ "greeting": format!("Hello, {}!", text) }))
                    .unwrap();
            OutputStream::respond(output)
        }
    }

    w.register_block("test/uppercase", Arc::new(UppercaseBlock))
        .unwrap();
    w.register_block("test/greet", Arc::new(GreetBlock))
        .unwrap();

    let flow_json = r#"{
        "id": "greet-flow",
        "name": "Greeting Pipeline",
        "version": "0.1.0",
        "steps": [
            {
                "id": "upper",
                "block": "test/uppercase",
                "input": { "text": "$.input.name" }
            },
            {
                "id": "greet",
                "block": "test/greet",
                "input": { "text": "$.upper.text" }
            }
        ]
    }"#;

    w.add_flow_json(flow_json).expect("add flow json failed");
    w.seal().await.expect("seal failed");

    // Set up input as body bytes
    let input = serde_json::to_vec(&serde_json::json!({ "name": "world" })).unwrap();
    let result = run_flow(&w, "greet-flow", Message::new("pipeline"), input).await;

    assert!(result.is_respond(), "expected respond, got: {result:?}");
    let output: serde_json::Value = serde_json::from_slice(result.body()).unwrap();
    assert_eq!(output, serde_json::json!({ "greeting": "Hello, WORLD!" }));
}

#[tokio::test]
async fn test_waferflow_conditional_routing() {
    let mut w = empty_wafer();

    struct CheckSignBlock;
    #[async_trait::async_trait]
    impl Block for CheckSignBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/check-sign", "0.0.1", "http-handler@v1", "CheckSign")
                .instance_mode(InstanceMode::Singleton)
        }
        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            input: InputStream,
        ) -> OutputStream {
            let bytes = input.collect_to_bytes().await;
            let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
            let n = val.get("n").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let output = serde_json::to_vec(&serde_json::json!({ "positive": n > 0.0 })).unwrap();
            OutputStream::respond(output)
        }
    }

    struct FormatPositiveBlock;
    #[async_trait::async_trait]
    impl Block for FormatPositiveBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new(
                "test/format-positive",
                "0.0.1",
                "http-handler@v1",
                "FormatPositive",
            )
            .instance_mode(InstanceMode::Singleton)
        }
        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            let output = serde_json::to_vec(&serde_json::json!({ "result": "positive" })).unwrap();
            OutputStream::respond(output)
        }
    }

    struct FormatNegativeBlock;
    #[async_trait::async_trait]
    impl Block for FormatNegativeBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new(
                "test/format-negative",
                "0.0.1",
                "http-handler@v1",
                "FormatNegative",
            )
            .instance_mode(InstanceMode::Singleton)
        }
        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            let output =
                serde_json::to_vec(&serde_json::json!({ "result": "non-positive" })).unwrap();
            OutputStream::respond(output)
        }
    }

    w.register_block("test/check-sign", Arc::new(CheckSignBlock))
        .unwrap();
    w.register_block("test/format-positive", Arc::new(FormatPositiveBlock))
        .unwrap();
    w.register_block("test/format-negative", Arc::new(FormatNegativeBlock))
        .unwrap();

    let flow_json = r#"{
        "id": "sign-check",
        "name": "Sign Check",
        "version": "0.1.0",
        "steps": [
            {
                "id": "check",
                "block": "test/check-sign",
                "input": { "n": "$.input.number" },
                "next": [
                    { "when": "$.check.positive == true", "step": "pos" },
                    { "step": "neg" }
                ]
            },
            {
                "id": "pos",
                "block": "test/format-positive",
                "input": {}
            },
            {
                "id": "neg",
                "block": "test/format-negative",
                "input": {}
            }
        ]
    }"#;

    w.add_flow_json(flow_json).expect("add flow json failed");
    w.seal().await.expect("seal failed");

    // Test positive
    let input = serde_json::to_vec(&serde_json::json!({ "number": 42 })).unwrap();
    let result = run_flow(&w, "sign-check", Message::new("pipeline"), input).await;
    assert!(
        result.is_respond(),
        "expected respond (positive), got: {result:?}"
    );
    let output: serde_json::Value = serde_json::from_slice(result.body()).unwrap();
    assert_eq!(output, serde_json::json!({ "result": "positive" }));

    // Test negative
    let input = serde_json::to_vec(&serde_json::json!({ "number": -5 })).unwrap();
    let result = run_flow(&w, "sign-check", Message::new("pipeline"), input).await;
    assert!(
        result.is_respond(),
        "expected respond (negative), got: {result:?}"
    );
    let output: serde_json::Value = serde_json::from_slice(result.body()).unwrap();
    assert_eq!(output, serde_json::json!({ "result": "non-positive" }));
}

#[tokio::test]
async fn test_waferflow_validation_errors() {
    let mut w = empty_wafer();

    // Duplicate step IDs
    let result = w.add_flow_json(
        r#"{
        "id": "bad",
        "name": "Bad",
        "version": "0.1.0",
        "steps": [
            { "id": "a", "block": "x" },
            { "id": "a", "block": "y" }
        ]
    }"#,
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("duplicate"));
}

#[tokio::test]
async fn test_waferflow_max_steps_limit() {
    let mut w = empty_wafer();

    struct LoopBlock;
    #[async_trait::async_trait]
    impl Block for LoopBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/loop-block", "0.0.1", "http-handler@v1", "LoopBlock")
                .instance_mode(InstanceMode::Singleton)
        }
        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            let output = serde_json::to_vec(&serde_json::json!({ "ok": true })).unwrap();
            OutputStream::respond(output)
        }
    }

    w.register_block("test/loop-block", Arc::new(LoopBlock))
        .unwrap();

    let flow_json = r#"{
        "id": "infinite",
        "name": "Infinite",
        "version": "0.1.0",
        "config": { "max_steps": 5 },
        "steps": [
            {
                "id": "loop",
                "block": "test/loop-block",
                "input": {},
                "next": [{ "step": "loop" }]
            }
        ]
    }"#;

    w.add_flow_json(flow_json).expect("add flow json failed");
    w.seal().await.expect("seal failed");

    let result = run_flow(&w, "infinite", Message::new("test"), vec![]).await;
    assert!(result.is_error(), "expected error, got: {result:?}");
    assert!(
        result.error().message.contains("max steps"),
        "error: {:?}",
        result.error().message
    );
}

#[tokio::test]
async fn test_waferflow_not_found() {
    let w = empty_wafer();
    let result = run_flow(&w, "nonexistent", Message::new("test"), vec![]).await;
    assert!(result.is_error(), "expected error, got: {result:?}");
    assert!(
        result.error().message.contains("not found"),
        "error: {:?}",
        result.error().message
    );
}
