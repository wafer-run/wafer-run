use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use wafer_run::*;

// ---------------------------------------------------------------------------
// Helper: build a simple Node pointing at a registered block
// ---------------------------------------------------------------------------
fn block_node(block: &str) -> Box<Node> {
    let mut n = Node::new();
    n.block = block.to_string();
    Box::new(n)
}

fn block_node_with_config(block: &str, config: serde_json::Value) -> Box<Node> {
    let mut n = Node::new();
    n.block = block.to_string();
    n.config = Some(config);
    Box::new(n)
}

fn match_node(block: &str, pattern: &str) -> Box<Node> {
    let mut n = Node::new();
    n.block = block.to_string();
    n.match_pattern = pattern.to_string();
    Box::new(n)
}

fn chain_ref_node(chain_id: &str) -> Box<Node> {
    let mut n = Node::new();
    n.chain = chain_id.to_string();
    Box::new(n)
}

fn make_chain(id: &str, root: Box<Node>) -> Chain {
    Chain {
        id: id.to_string(),
        summary: format!("Test chain: {}", id),
        config: ChainConfig::default(),
        root,
    }
}

fn make_chain_with_on_error(id: &str, root: Box<Node>, on_error: &str) -> Chain {
    Chain {
        id: id.to_string(),
        summary: format!("Test chain: {}", id),
        config: ChainConfig {
            on_error: on_error.to_string(),
            timeout: Duration::ZERO,
        },
        root,
    }
}

// ===========================================================================
// 1. Basic runtime creation and inline block registration
// ===========================================================================

#[test]
fn test_create_runtime() {
    let w = Wafer::new();
    assert!(w.chains_info().is_empty());
}

#[test]
fn test_register_inline_block() {
    let mut w = Wafer::new();
    w.register_block_func("echo", |_ctx, msg| msg.clone().cont());
    assert!(w.has_block("echo"));
    assert!(!w.has_block("nonexistent"));
}

// ===========================================================================
// 2. Build and execute a single-node chain
// ===========================================================================

#[test]
fn test_single_block_chain() {
    let mut w = Wafer::new();

    w.register_block_func("upper", |_ctx, msg| {
        let text = String::from_utf8_lossy(&msg.data).to_uppercase();
        let mut out = msg.clone();
        out.data = text.into_bytes();
        out.cont()
    });

    let root = block_node("upper");
    w.add_chain(make_chain("to-upper", root));
    w.resolve().expect("resolve failed");

    let mut msg = Message::new("text", "hello world");
    let result = w.execute("to-upper", &mut msg);

    assert_eq!(result.action, Action::Continue);
    assert_eq!(
        String::from_utf8_lossy(&msg.data),
        "HELLO WORLD"
    );
}

// ===========================================================================
// 3. Multi-block sequential chain (a -> b -> c)
// ===========================================================================

#[test]
fn test_sequential_chain() {
    let mut w = Wafer::new();

    // Block A: append "-A"
    w.register_block_func("append-a", |_ctx, msg| {
        let mut out = msg.clone();
        let mut text = String::from_utf8_lossy(&out.data).to_string();
        text.push_str("-A");
        out.data = text.into_bytes();
        out.cont()
    });

    // Block B: append "-B"
    w.register_block_func("append-b", |_ctx, msg| {
        let mut out = msg.clone();
        let mut text = String::from_utf8_lossy(&out.data).to_string();
        text.push_str("-B");
        out.data = text.into_bytes();
        out.cont()
    });

    // Block C: append "-C"
    w.register_block_func("append-c", |_ctx, msg| {
        let mut out = msg.clone();
        let mut text = String::from_utf8_lossy(&out.data).to_string();
        text.push_str("-C");
        out.data = text.into_bytes();
        out.cont()
    });

    // Build chain: A -> B -> C
    let mut root = block_node("append-a");
    let mut node_b = block_node("append-b");
    let node_c = block_node("append-c");
    node_b.next.push(node_c);
    root.next.push(node_b);

    w.add_chain(make_chain("abc", root));
    w.resolve().expect("resolve failed");

    let mut msg = Message::new("test", "start");
    let result = w.execute("abc", &mut msg);

    assert_eq!(result.action, Action::Continue);
    assert_eq!(String::from_utf8_lossy(&msg.data), "start-A-B-C");
}

// ===========================================================================
// 4. Pattern matching (first-match siblings)
// ===========================================================================

#[test]
fn test_pattern_matching_exact() {
    let mut w = Wafer::new();

    w.register_block_func("create-handler", |_ctx, msg| {
        let mut out = msg.clone();
        out.data = b"created".to_vec();
        out.cont()
    });

    w.register_block_func("delete-handler", |_ctx, msg| {
        let mut out = msg.clone();
        out.data = b"deleted".to_vec();
        out.cont()
    });

    w.register_block_func("passthrough", |_ctx, msg| msg.clone().cont());

    // Root -> match "user.create" | match "user.delete" | fallback
    let mut root = block_node("passthrough");
    root.next.push(match_node("create-handler", "user.create"));
    root.next.push(match_node("delete-handler", "user.delete"));

    w.add_chain(make_chain("dispatch", root));
    w.resolve().expect("resolve failed");

    // Test user.create
    let mut msg = Message::new("user.create", "");
    let result = w.execute("dispatch", &mut msg);
    assert_eq!(result.action, Action::Continue);
    assert_eq!(String::from_utf8_lossy(&msg.data), "created");

    // Test user.delete
    let mut msg = Message::new("user.delete", "");
    let result = w.execute("dispatch", &mut msg);
    assert_eq!(result.action, Action::Continue);
    assert_eq!(String::from_utf8_lossy(&msg.data), "deleted");
}

#[test]
fn test_pattern_matching_wildcard() {
    let mut w = Wafer::new();

    w.register_block_func("user-handler", |_ctx, msg| {
        let mut out = msg.clone();
        out.data = b"user-matched".to_vec();
        out.cont()
    });

    w.register_block_func("fallback", |_ctx, msg| {
        let mut out = msg.clone();
        out.data = b"fallback-matched".to_vec();
        out.cont()
    });

    w.register_block_func("noop", |_ctx, msg| msg.clone().cont());

    let mut root = block_node("noop");
    root.next.push(match_node("user-handler", "user.*"));
    root.next.push(match_node("fallback", "")); // empty = always matches

    w.add_chain(make_chain("wildcard", root));
    w.resolve().expect("resolve failed");

    // "user.update" matches "user.*"
    let mut msg = Message::new("user.update", "");
    let result = w.execute("wildcard", &mut msg);
    assert_eq!(String::from_utf8_lossy(&msg.data), "user-matched");
    assert_eq!(result.action, Action::Continue);

    // "order.create" does not match "user.*", falls to fallback
    let mut msg = Message::new("order.create", "");
    let result = w.execute("wildcard", &mut msg);
    assert_eq!(String::from_utf8_lossy(&msg.data), "fallback-matched");
    assert_eq!(result.action, Action::Continue);
}

#[test]
fn test_pattern_matching_double_wildcard() {
    let mut w = Wafer::new();

    w.register_block_func("deep-handler", |_ctx, msg| {
        let mut out = msg.clone();
        out.data = b"deep".to_vec();
        out.cont()
    });

    w.register_block_func("noop", |_ctx, msg| msg.clone().cont());

    let mut root = block_node("noop");
    root.next.push(match_node("deep-handler", "event.**"));

    w.add_chain(make_chain("deep-match", root));
    w.resolve().expect("resolve failed");

    // "event.user.created" matches "event.**"
    let mut msg = Message::new("event.user.created", "");
    let result = w.execute("deep-match", &mut msg);
    assert_eq!(result.action, Action::Continue);
    assert_eq!(String::from_utf8_lossy(&msg.data), "deep");

    // "other.thing" does NOT match "event.**", no match so continue
    let mut msg = Message::new("other.thing", "");
    let result = w.execute("deep-match", &mut msg);
    assert_eq!(result.action, Action::Continue);
    assert_eq!(String::from_utf8_lossy(&msg.data), ""); // unchanged
}

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

#[test]
fn test_router_basic() {
    let mut router = Router::new();

    router.retrieve("/items", |_ctx, msg| {
        json_respond(msg.clone(), 200, &serde_json::json!({"items": []}))
    });

    router.create("/items", |_ctx, msg| {
        json_respond(msg.clone(), 201, &serde_json::json!({"id": "new-1"}))
    });

    router.retrieve("/items/{id}", |_ctx, msg| {
        let id = msg.var("id").to_string();
        json_respond(msg.clone(), 200, &serde_json::json!({"id": id}))
    });

    // Simulate a GET /items request
    let ctx = make_test_context();
    let mut msg = Message::new("http.request", "");
    msg.set_meta("req.action", "retrieve");
    msg.set_meta("req.resource", "/items");

    let result = router.route(&ctx, &mut msg);
    assert_eq!(result.action, Action::Respond);
    let body: serde_json::Value =
        serde_json::from_slice(&result.response.unwrap().data).unwrap();
    assert_eq!(body["items"], serde_json::json!([]));

    // Simulate POST /items
    let mut msg = Message::new("http.request", "");
    msg.set_meta("req.action", "create");
    msg.set_meta("req.resource", "/items");

    let result = router.route(&ctx, &mut msg);
    assert_eq!(result.action, Action::Respond);
    let body: serde_json::Value =
        serde_json::from_slice(&result.response.unwrap().data).unwrap();
    assert_eq!(body["id"], "new-1");

    // Simulate GET /items/42
    let mut msg = Message::new("http.request", "");
    msg.set_meta("req.action", "retrieve");
    msg.set_meta("req.resource", "/items/42");

    let result = router.route(&ctx, &mut msg);
    assert_eq!(result.action, Action::Respond);
    let body: serde_json::Value =
        serde_json::from_slice(&result.response.unwrap().data).unwrap();
    assert_eq!(body["id"], "42");
}

#[test]
fn test_router_not_found() {
    let router = Router::new();
    let ctx = make_test_context();

    let mut msg = Message::new("http.request", "");
    msg.set_meta("req.action", "retrieve");
    msg.set_meta("req.resource", "/nonexistent");

    let result = router.route(&ctx, &mut msg);
    assert_eq!(result.action, Action::Error);
    assert_eq!(result.error.as_ref().unwrap().code, "not_found");
}

#[test]
fn test_router_update_delete() {
    let mut router = Router::new();

    router.update("/items/{id}", |_ctx, msg| {
        let id = msg.var("id").to_string();
        json_respond(msg.clone(), 200, &serde_json::json!({"updated": id}))
    });

    router.delete("/items/{id}", |_ctx, msg| {
        respond(msg.clone(), 204, Vec::new(), "")
    });

    let ctx = make_test_context();

    // Update
    let mut msg = Message::new("http.request", "");
    msg.set_meta("req.action", "update");
    msg.set_meta("req.resource", "/items/99");

    let result = router.route(&ctx, &mut msg);
    assert_eq!(result.action, Action::Respond);
    let body: serde_json::Value =
        serde_json::from_slice(&result.response.unwrap().data).unwrap();
    assert_eq!(body["updated"], "99");

    // Delete
    let mut msg = Message::new("http.request", "");
    msg.set_meta("req.action", "delete");
    msg.set_meta("req.resource", "/items/99");

    let result = router.route(&ctx, &mut msg);
    assert_eq!(result.action, Action::Respond);
    assert!(result.response.unwrap().data.is_empty());
}

// ===========================================================================
// 6. Helper functions (respond, error, json_respond, ResponseBuilder)
// ===========================================================================

#[test]
fn test_respond_helper() {
    let msg = Message::new("test", "payload");
    let result = respond(msg, 200, b"ok".to_vec(), "text/plain");

    assert_eq!(result.action, Action::Respond);
    let resp = result.response.unwrap();
    assert_eq!(resp.data, b"ok");
    assert_eq!(resp.meta.get("resp.status").unwrap(), "200");
    assert_eq!(resp.meta.get("resp.content_type").unwrap(), "text/plain");
}

#[test]
fn test_error_helper() {
    let msg = Message::new("test", "");
    let result = error(msg, 400, "bad_request", "missing field");

    assert_eq!(result.action, Action::Error);
    let err = result.error.unwrap();
    assert_eq!(err.code, "bad_request");
    assert_eq!(err.message, "missing field");
    assert_eq!(err.meta.get("resp.status").unwrap(), "400");
}

#[test]
fn test_json_respond_helper() {
    #[derive(serde::Serialize)]
    struct Item {
        id: u32,
        name: String,
    }

    let msg = Message::new("test", "");
    let item = Item {
        id: 1,
        name: "Widget".to_string(),
    };
    let result = json_respond(msg, 200, &item);

    assert_eq!(result.action, Action::Respond);
    let resp = result.response.unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&resp.data).unwrap();
    assert_eq!(parsed["id"], 1);
    assert_eq!(parsed["name"], "Widget");
    assert_eq!(resp.meta.get("resp.content_type").unwrap(), "application/json");
}

#[test]
fn test_standard_error_helpers() {
    let msg = Message::new("test", "");
    let r = err_bad_request(msg, "bad");
    assert_eq!(r.error.as_ref().unwrap().code, "invalid_argument");
    assert_eq!(r.error.as_ref().unwrap().meta.get("resp.status").unwrap(), "400");

    let msg = Message::new("test", "");
    let r = err_unauthorized(msg, "no auth");
    assert_eq!(r.error.as_ref().unwrap().code, "unauthenticated");

    let msg = Message::new("test", "");
    let r = err_forbidden(msg, "denied");
    assert_eq!(r.error.as_ref().unwrap().code, "permission_denied");

    let msg = Message::new("test", "");
    let r = err_not_found(msg, "gone");
    assert_eq!(r.error.as_ref().unwrap().code, "not_found");

    let msg = Message::new("test", "");
    let r = err_conflict(msg, "exists");
    assert_eq!(r.error.as_ref().unwrap().code, "already_exists");

    let msg = Message::new("test", "");
    let r = err_validation(msg, "invalid");
    assert_eq!(r.error.as_ref().unwrap().code, "invalid_argument");

    let msg = Message::new("test", "");
    let r = err_internal(msg, "oops");
    assert_eq!(r.error.as_ref().unwrap().code, "internal");
}

#[test]
fn test_response_builder() {
    let msg = Message::new("test", "");

    let result = new_response(msg, 200)
        .set_header("X-Request-Id", "abc-123")
        .set_cookie("session=xyz; Path=/; HttpOnly")
        .set_cookie("theme=dark; Path=/")
        .json(&serde_json::json!({"ok": true}));

    assert_eq!(result.action, Action::Respond);
    let resp = result.response.unwrap();

    let parsed: serde_json::Value = serde_json::from_slice(&resp.data).unwrap();
    assert_eq!(parsed["ok"], true);
    assert_eq!(resp.meta.get("resp.content_type").unwrap(), "application/json");
    assert_eq!(resp.meta.get("resp.status").unwrap(), "200");
    assert_eq!(
        resp.meta.get("resp.header.X-Request-Id").unwrap(),
        "abc-123"
    );
    assert_eq!(
        resp.meta.get("resp.set_cookie.0").unwrap(),
        "session=xyz; Path=/; HttpOnly"
    );
    assert_eq!(
        resp.meta.get("resp.set_cookie.1").unwrap(),
        "theme=dark; Path=/"
    );
}

#[test]
fn test_response_builder_body() {
    let msg = Message::new("test", "");
    let result = new_response(msg, 201)
        .body(b"raw bytes here".to_vec(), "application/octet-stream");

    assert_eq!(result.action, Action::Respond);
    let resp = result.response.unwrap();
    assert_eq!(resp.data, b"raw bytes here");
    assert_eq!(
        resp.meta.get("resp.content_type").unwrap(),
        "application/octet-stream"
    );
}

// ===========================================================================
// 7. Observability hooks
// ===========================================================================

#[test]
fn test_observability_chain_hooks() {
    let mut w = Wafer::new();

    let chain_start_count = Arc::new(AtomicUsize::new(0));
    let chain_end_count = Arc::new(AtomicUsize::new(0));
    let cs = chain_start_count.clone();
    let ce = chain_end_count.clone();

    w.hooks.on_chain_start(move |_chain_id, _msg| {
        cs.fetch_add(1, Ordering::SeqCst);
    });
    w.hooks.on_chain_end(move |_chain_id, _result, _dur| {
        ce.fetch_add(1, Ordering::SeqCst);
    });

    w.register_block_func("noop", |_ctx, msg| msg.clone().cont());

    let root = block_node("noop");
    w.add_chain(make_chain("observed", root));
    w.resolve().expect("resolve failed");

    let mut msg = Message::new("test", "data");
    w.execute("observed", &mut msg);

    assert_eq!(chain_start_count.load(Ordering::SeqCst), 1);
    assert_eq!(chain_end_count.load(Ordering::SeqCst), 1);

    // Execute again
    w.execute("observed", &mut msg);
    assert_eq!(chain_start_count.load(Ordering::SeqCst), 2);
    assert_eq!(chain_end_count.load(Ordering::SeqCst), 2);
}

#[test]
fn test_observability_block_hooks() {
    let mut w = Wafer::new();

    let block_names = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let bn = block_names.clone();

    w.hooks.on_block_start(move |ctx| {
        bn.lock().push(ctx.block_name.clone());
    });

    let block_durations = Arc::new(parking_lot::Mutex::new(Vec::<Duration>::new()));
    let bd = block_durations.clone();

    w.hooks
        .on_block_end(move |_ctx, _result, duration| {
            bd.lock().push(duration);
        });

    w.register_block_func("step-1", |_ctx, msg| msg.clone().cont());
    w.register_block_func("step-2", |_ctx, msg| msg.clone().cont());

    let mut root = block_node("step-1");
    root.next.push(block_node("step-2"));

    w.add_chain(make_chain("two-steps", root));
    w.resolve().expect("resolve failed");

    let mut msg = Message::new("test", "");
    w.execute("two-steps", &mut msg);

    let names = block_names.lock();
    assert_eq!(names.len(), 2);
    assert_eq!(names[0], "step-1");
    assert_eq!(names[1], "step-2");

    let durations = block_durations.lock();
    assert_eq!(durations.len(), 2);
}

// ===========================================================================
// 8. Chain references
// ===========================================================================

#[test]
fn test_chain_reference() {
    let mut w = Wafer::new();

    w.register_block_func("validate", |_ctx, msg| {
        let mut out = msg.clone();
        out.set_meta("validated", "true");
        out.cont()
    });

    w.register_block_func("store", |_ctx, msg| {
        let mut out = msg.clone();
        out.data = b"stored".to_vec();
        out.cont()
    });

    // Inner chain: validate
    let validate_root = block_node("validate");
    w.add_chain(make_chain("validation-chain", validate_root));

    // Outer chain: ref to validation-chain -> store
    let mut ref_node = chain_ref_node("validation-chain");
    ref_node.next.push(block_node("store"));

    w.add_chain(make_chain("main-chain", ref_node));
    w.resolve().expect("resolve failed");

    let mut msg = Message::new("user.create", "data");
    let result = w.execute("main-chain", &mut msg);

    assert_eq!(result.action, Action::Continue);
    assert_eq!(msg.get_meta("validated"), "true");
    assert_eq!(String::from_utf8_lossy(&msg.data), "stored");
}

#[test]
fn test_chain_reference_short_circuit() {
    let mut w = Wafer::new();

    // The inner chain responds immediately (short-circuits)
    w.register_block_func("responder", |_ctx, msg| {
        respond(msg.clone(), 200, b"early-response".to_vec(), "text/plain")
    });

    w.register_block_func("should-not-run", |_ctx, msg| {
        let mut out = msg.clone();
        out.data = b"this should never appear".to_vec();
        out.cont()
    });

    let responder_root = block_node("responder");
    w.add_chain(make_chain("early-exit", responder_root));

    let mut ref_node = chain_ref_node("early-exit");
    ref_node.next.push(block_node("should-not-run"));

    w.add_chain(make_chain("outer", ref_node));
    w.resolve().expect("resolve failed");

    let mut msg = Message::new("test", "");
    let result = w.execute("outer", &mut msg);

    assert_eq!(result.action, Action::Respond);
    let resp = result.response.unwrap();
    assert_eq!(resp.data, b"early-response");
}

#[test]
fn test_chain_reference_not_found() {
    let mut w = Wafer::new();

    w.register_block_func("noop", |_ctx, msg| msg.clone().cont());

    // Create a chain that references a non-existent chain
    let mut ref_node = chain_ref_node("does-not-exist");
    ref_node.next.push(block_node("noop"));

    w.add_chain(make_chain("bad-ref", ref_node));
    // We register noop so the outer node resolves fine
    w.resolve().expect("resolve failed");

    let mut msg = Message::new("test", "");
    let result = w.execute("bad-ref", &mut msg);

    assert_eq!(result.action, Action::Error);
    assert!(result
        .error
        .as_ref()
        .unwrap()
        .message
        .contains("does-not-exist"));
}

// ===========================================================================
// 9. Error handling: on_error = stop vs continue
// ===========================================================================

#[test]
fn test_on_error_stop() {
    let mut w = Wafer::new();

    w.register_block_func("fail", |_ctx, msg| {
        msg.clone()
            .err(WaferError::new("test_error", "intentional failure"))
    });

    w.register_block_func("after-fail", |_ctx, msg| {
        let mut out = msg.clone();
        out.data = b"should-not-run".to_vec();
        out.cont()
    });

    let mut root = block_node("fail");
    root.next.push(block_node("after-fail"));

    w.add_chain(make_chain_with_on_error("stop-chain", root, "stop"));
    w.resolve().expect("resolve failed");

    let mut msg = Message::new("test", "");
    let result = w.execute("stop-chain", &mut msg);

    assert_eq!(result.action, Action::Error);
    assert_eq!(result.error.as_ref().unwrap().code, "test_error");
    // "after-fail" should NOT have run
    assert_ne!(String::from_utf8_lossy(&msg.data), "should-not-run");
}

#[test]
fn test_on_error_continue() {
    let mut w = Wafer::new();

    let fail_count = Arc::new(AtomicUsize::new(0));
    let fc = fail_count.clone();

    w.register_block_func("fail", move |_ctx, msg| {
        fc.fetch_add(1, Ordering::SeqCst);
        msg.clone()
            .err(WaferError::new("test_error", "intentional failure"))
    });

    w.register_block_func("after-fail", |_ctx, msg| {
        let mut out = msg.clone();
        out.data = b"recovered".to_vec();
        out.cont()
    });

    let mut root = block_node("fail");
    root.next.push(block_node("after-fail"));

    w.add_chain(make_chain_with_on_error("cont-chain", root, "continue"));
    w.resolve().expect("resolve failed");

    let mut msg = Message::new("test", "");
    let result = w.execute("cont-chain", &mut msg);

    // With on_error=continue, the chain proceeds past the error
    assert_eq!(fail_count.load(Ordering::SeqCst), 1);
    assert_eq!(result.action, Action::Continue);
    assert_eq!(String::from_utf8_lossy(&msg.data), "recovered");
}

#[test]
fn test_on_error_continue_no_more_nodes() {
    let mut w = Wafer::new();

    w.register_block_func("fail-at-end", |_ctx, msg| {
        msg.clone()
            .err(WaferError::new("terminal_error", "error at tail"))
    });

    let root = block_node("fail-at-end");
    w.add_chain(make_chain_with_on_error("cont-end", root, "continue"));
    w.resolve().expect("resolve failed");

    let mut msg = Message::new("test", "");
    let result = w.execute("cont-end", &mut msg);

    // on_error=continue with no more nodes means the error is swallowed
    // and the runtime returns Continue
    assert_eq!(result.action, Action::Continue);
}

// ===========================================================================
// 10. Respond short-circuits the chain
// ===========================================================================

#[test]
fn test_respond_short_circuits() {
    let mut w = Wafer::new();

    w.register_block_func("early-respond", |_ctx, msg| {
        respond(msg.clone(), 200, b"early".to_vec(), "text/plain")
    });

    w.register_block_func("unreachable", |_ctx, msg| {
        let mut out = msg.clone();
        out.data = b"unreachable".to_vec();
        out.cont()
    });

    let mut root = block_node("early-respond");
    root.next.push(block_node("unreachable"));

    w.add_chain(make_chain("short-circuit", root));
    w.resolve().expect("resolve failed");

    let mut msg = Message::new("test", "");
    let result = w.execute("short-circuit", &mut msg);

    assert_eq!(result.action, Action::Respond);
    assert_eq!(result.response.unwrap().data, b"early");
}

// ===========================================================================
// 11. Drop action
// ===========================================================================

#[test]
fn test_drop_action() {
    let mut w = Wafer::new();

    w.register_block_func("dropper", |_ctx, msg| msg.clone().drop_msg());

    w.register_block_func("unreachable", |_ctx, msg| {
        let mut out = msg.clone();
        out.data = b"unreachable".to_vec();
        out.cont()
    });

    let mut root = block_node("dropper");
    root.next.push(block_node("unreachable"));

    w.add_chain(make_chain("drop-chain", root));
    w.resolve().expect("resolve failed");

    let mut msg = Message::new("test", "data");
    let result = w.execute("drop-chain", &mut msg);

    assert_eq!(result.action, Action::Drop);
}

// ===========================================================================
// 12. Chain not found
// ===========================================================================

#[test]
fn test_execute_nonexistent_chain() {
    let w = Wafer::new();

    let mut msg = Message::new("test", "data");
    let result = w.execute("nonexistent", &mut msg);

    assert_eq!(result.action, Action::Error);
    assert_eq!(result.error.as_ref().unwrap().code, "chain_not_found");
}

// ===========================================================================
// 13. Block with config
// ===========================================================================

#[test]
fn test_block_with_config() {
    let mut w = Wafer::new();

    w.register_block_func("configurable", |ctx, msg| {
        let prefix = ctx.config_get("prefix").unwrap_or("default");
        let mut out = msg.clone();
        let text = format!("{}-{}", prefix, String::from_utf8_lossy(&out.data));
        out.data = text.into_bytes();
        out.cont()
    });

    let root = block_node_with_config(
        "configurable",
        serde_json::json!({"prefix": "hello"}),
    );

    w.add_chain(make_chain("config-chain", root));
    w.resolve().expect("resolve failed");

    let mut msg = Message::new("test", "world");
    let result = w.execute("config-chain", &mut msg);

    assert_eq!(result.action, Action::Continue);
    assert_eq!(String::from_utf8_lossy(&msg.data), "hello-world");
}

// ===========================================================================
// 14. Message methods
// ===========================================================================

#[test]
fn test_message_methods() {
    let mut msg = Message::new("user.create", r#"{"name":"Alice"}"#);
    msg.set_meta("req.action", "create");
    msg.set_meta("req.resource", "/users");
    msg.set_meta("req.param.id", "42");
    msg.set_meta("req.query.page", "2");
    msg.set_meta("req.content_type", "application/json");
    msg.set_meta("auth.user_id", "user-1");
    msg.set_meta("auth.user_email", "alice@example.com");
    msg.set_meta("auth.user_roles", "admin,editor");
    msg.set_meta("req.client.ip", "127.0.0.1");
    msg.set_meta("http.header.Cookie", "session=abc; theme=dark");

    assert_eq!(msg.action(), "create");
    assert_eq!(msg.path(), "/users");
    assert_eq!(msg.var("id"), "42");
    assert_eq!(msg.query("page"), "2");
    assert_eq!(msg.content_type(), "application/json");
    assert_eq!(msg.user_id(), "user-1");
    assert_eq!(msg.user_email(), "alice@example.com");
    assert_eq!(msg.user_roles(), vec!["admin", "editor"]);
    assert!(msg.is_admin());
    assert_eq!(msg.remote_addr(), "127.0.0.1");
    assert_eq!(msg.cookie("session"), "abc");
    assert_eq!(msg.cookie("theme"), "dark");
    assert_eq!(msg.cookie("nonexistent"), "");
    assert_eq!(msg.body(), b"{\"name\":\"Alice\"}");

    let params = msg.query_params();
    assert_eq!(*params.get("page").unwrap(), "2");

    let (page, page_size, offset) = msg.pagination_params(20);
    assert_eq!(page, 2);
    assert_eq!(page_size, 20);
    assert_eq!(offset, 20);

    // Decode/unmarshal
    #[derive(serde::Deserialize)]
    struct User {
        name: String,
    }
    let user: User = msg.decode().unwrap();
    assert_eq!(user.name, "Alice");

    // SetData
    #[derive(serde::Serialize)]
    struct Response {
        ok: bool,
    }
    msg.set_data(&Response { ok: true }).unwrap();
    assert_eq!(String::from_utf8_lossy(&msg.data), r#"{"ok":true}"#);
}

// ===========================================================================
// 15. Resolve errors
// ===========================================================================

#[test]
fn test_resolve_missing_block() {
    let mut w = Wafer::new();

    let root = block_node("unregistered-block");
    w.add_chain(make_chain("broken", root));

    let err = w.resolve().unwrap_err();
    assert!(err.contains("unregistered-block"), "Error: {}", err);
}

// ===========================================================================
// 16. ChainDef (JSON-based chain definition)
// ===========================================================================

#[test]
fn test_add_chain_def() {
    let mut w = Wafer::new();

    w.register_block_func("echo", |_ctx, msg| msg.clone().cont());

    let def = ChainDef {
        id: "from-def".to_string(),
        summary: "Defined from JSON".to_string(),
        config: ChainConfigDef {
            on_error: "stop".to_string(),
            timeout: "30s".to_string(),
        },
        root: NodeDef {
            block: "echo".to_string(),
            chain: String::new(),
            r#match: String::new(),
            config: None,
            instance: String::new(),
            next: vec![],
        },
    };

    w.add_chain_def(&def);
    w.resolve().expect("resolve failed");

    let mut msg = Message::new("test", "hello");
    let result = w.execute("from-def", &mut msg);
    assert_eq!(result.action, Action::Continue);
}

// ===========================================================================
// 17. Panic recovery
// ===========================================================================

#[test]
fn test_panic_recovery() {
    let mut w = Wafer::new();

    w.register_block_func("panicker", |_ctx, _msg| {
        panic!("block went wrong");
    });

    let root = block_node("panicker");
    w.add_chain(make_chain("panic-chain", root));
    w.resolve().expect("resolve failed");

    let mut msg = Message::new("test", "");
    let result = w.execute("panic-chain", &mut msg);

    assert_eq!(result.action, Action::Error);
    let err = result.error.unwrap();
    assert_eq!(err.code, "panic");
    assert!(err.message.contains("block went wrong"));
}

// ===========================================================================
// 18. Multiple chains info
// ===========================================================================

#[test]
fn test_chains_info() {
    let mut w = Wafer::new();

    w.register_block_func("noop", |_ctx, msg| msg.clone().cont());

    w.add_chain(make_chain("chain-a", block_node("noop")));
    w.add_chain(make_chain("chain-b", block_node("noop")));

    let info = w.chains_info();
    assert_eq!(info.len(), 2);

    let ids: Vec<&str> = info.iter().map(|c| c.id.as_str()).collect();
    assert!(ids.contains(&"chain-a"));
    assert!(ids.contains(&"chain-b"));
}

// ===========================================================================
// 19. Start / Stop lifecycle
// ===========================================================================

#[test]
fn test_start_and_stop() {
    let mut w = Wafer::new();

    w.register_block_func("lifecycle-block", |_ctx, msg| msg.clone().cont());

    let root = block_node("lifecycle-block");
    w.add_chain(make_chain("lifecycle-test", root));

    // Start implicitly resolves if not already resolved
    w.start().expect("start failed");

    let mut msg = Message::new("test", "");
    let result = w.execute("lifecycle-test", &mut msg);
    assert_eq!(result.action, Action::Continue);

    // Stop calls lifecycle(Stop) on all resolved blocks
    w.stop();
}

// ===========================================================================
// 20. WaferError
// ===========================================================================

#[test]
fn test_wafer_error_display() {
    let err = WaferError::new("not_found", "user not found")
        .with_meta("resource", "users")
        .with_meta("id", "42");

    assert_eq!(err.to_string(), "not_found: user not found");
    assert_eq!(err.meta.get("resource").unwrap(), "users");
    assert_eq!(err.meta.get("id").unwrap(), "42");
}


// ===========================================================================
// Test context helper (minimal Context implementation for router tests)
// ===========================================================================

struct TestContext;

impl Context for TestContext {
    fn call_block(&self, _block_name: &str, _msg: &mut Message) -> Result_ {
        Result_ {
            action: Action::Continue,
            response: None,
            error: None,
            message: None,
        }
    }

    fn is_cancelled(&self) -> bool {
        false
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        None
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

    #[test]
    fn test_parse_versioned_block_valid() {
        let r = parse_versioned_block("github.com/acme/auth-block@v1.0.0").unwrap();
        assert_eq!(r.owner, "acme");
        assert_eq!(r.repo, "auth-block");
        assert_eq!(r.version, "v1.0.0");
    }

    #[test]
    fn test_parse_versioned_block_no_version() {
        assert!(parse_versioned_block("github.com/acme/auth-block").is_none());
    }

    #[test]
    fn test_parse_versioned_block_empty_version() {
        assert!(parse_versioned_block("github.com/acme/auth-block@").is_none());
    }

    #[test]
    fn test_parse_versioned_block_not_github() {
        assert!(parse_versioned_block("gitlab.com/acme/auth-block@v1.0.0").is_none());
    }

    #[test]
    fn test_parse_versioned_block_local_name() {
        assert!(parse_versioned_block("@wafer/auth").is_none());
    }

    #[test]
    fn test_parse_versioned_block_wrong_segments() {
        // Only 2 segments (missing repo)
        assert!(parse_versioned_block("github.com/acme@v1.0.0").is_none());
        // 4 segments (too many)
        assert!(parse_versioned_block("github.com/acme/auth/extra@v1.0.0").is_none());
    }

    #[test]
    fn test_parse_versioned_block_latest_rejected() {
        assert!(parse_versioned_block("github.com/acme/auth-block@latest").is_none());
    }

    #[test]
    fn test_parse_versioned_block_prerelease() {
        let r = parse_versioned_block("github.com/acme/auth-block@v2.0.0-rc.1").unwrap();
        assert_eq!(r.owner, "acme");
        assert_eq!(r.repo, "auth-block");
        assert_eq!(r.version, "v2.0.0-rc.1");
    }
}

// ===========================================================================
// 22b. parse_unversioned_block tests
// ===========================================================================

#[cfg(feature = "wasm")]
mod unversioned_block_tests {
    use wafer_run::parse_unversioned_block;

    #[test]
    fn test_parse_unversioned_block_valid() {
        let r = parse_unversioned_block("github.com/acme/auth-block").unwrap();
        assert_eq!(r.owner, "acme");
        assert_eq!(r.repo, "auth-block");
    }

    #[test]
    fn test_parse_unversioned_block_with_at_rejected() {
        assert!(parse_unversioned_block("github.com/acme/auth-block@v1.0.0").is_none());
    }

    #[test]
    fn test_parse_unversioned_block_not_github() {
        assert!(parse_unversioned_block("gitlab.com/acme/auth-block").is_none());
    }

    #[test]
    fn test_parse_unversioned_block_wrong_segments() {
        // Only 2 segments (missing repo)
        assert!(parse_unversioned_block("github.com/acme").is_none());
        // 4 segments (too many)
        assert!(parse_unversioned_block("github.com/acme/auth/extra").is_none());
        // 1 segment
        assert!(parse_unversioned_block("github.com").is_none());
    }

    #[test]
    fn test_parse_unversioned_block_at_latest() {
        let r = parse_unversioned_block("github.com/acme/auth-block@latest").unwrap();
        assert_eq!(r.owner, "acme");
        assert_eq!(r.repo, "auth-block");
    }

    #[test]
    fn test_parse_unversioned_block_empty_segments() {
        assert!(parse_unversioned_block("github.com//auth-block").is_none());
        assert!(parse_unversioned_block("github.com/acme/").is_none());
    }

    #[test]
    fn test_parse_unversioned_block_local_name() {
        assert!(parse_unversioned_block("my-block").is_none());
        assert!(parse_unversioned_block("@wafer/auth").is_none());
    }
}

// ===========================================================================
// 23. Remote block resolve error paths
// ===========================================================================

#[cfg(feature = "wasm")]
#[test]
fn test_resolve_versioned_block_download_error() {
    use wafer_run::*;

    let mut w = Wafer::new();

    let mut root = Node::new();
    // Use a nonexistent repo to trigger a download error
    root.block = "github.com/acme/nonexistent-block@v1.0.0".to_string();

    let chain = Chain {
        id: "remote-test".to_string(),
        summary: "test".to_string(),
        config: ChainConfig::default(),
        root: Box::new(root),
    };
    w.add_chain(chain);

    let err = w.resolve().unwrap_err();
    assert!(
        err.contains("failed to download") || err.contains("failed to load remote block"),
        "Expected download or load error, got: {}",
        err
    );
}

// ===========================================================================
// 24. Unversioned remote block resolve error paths
// ===========================================================================

#[cfg(feature = "wasm")]
#[test]
fn test_resolve_unversioned_block_download_error() {
    use wafer_run::*;

    let mut w = Wafer::new();

    let mut root = Node::new();
    // Use a nonexistent repo to trigger a download error
    root.block = "github.com/acme/nonexistent-block".to_string();

    let chain = Chain {
        id: "unversioned-test".to_string(),
        summary: "test".to_string(),
        config: ChainConfig::default(),
        root: Box::new(root),
    };
    w.add_chain(chain);

    let err = w.resolve().unwrap_err();
    assert!(
        err.contains("failed to fetch releases") || err.contains("failed to download") || err.contains("HTTP"),
        "Expected download error, got: {}",
        err
    );
}
