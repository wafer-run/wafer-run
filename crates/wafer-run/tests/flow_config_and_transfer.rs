//! Flow config is parsed, not trusted.
//!
//! - `add_flow_json` / `add_flow` refuse a flow whose config or routing the
//!   executor would otherwise misread: an `on_error` other than `"stop"` /
//!   `"continue"`, a malformed timeout, a `next` into a parallel branch, a
//!   transfer to the flow itself.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use serde_json::json;
use wafer_run::*;

/// Counts its invocations, waits `delay`, and passes the message through.
struct Counter {
    name: &'static str,
    calls: Arc<AtomicUsize>,
    delay: Duration,
}

#[async_trait::async_trait]
impl Block for Counter {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.0.1", "http-handler@v1", "counting fixture")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // Always yield, so a test's `tokio::time::timeout` can fire even
        // when a flow never ends.
        tokio::task::yield_now().await;
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        OutputStream::continue_with(msg)
    }
}

fn wafer() -> Wafer {
    Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer")
}

/// Register a [`Counter`] under `name`; returns its call count.
fn counter(w: &mut Wafer, name: &'static str, delay: Duration) -> Arc<AtomicUsize> {
    let calls = Arc::new(AtomicUsize::new(0));
    w.register_block(
        name,
        Arc::new(Counter {
            name,
            calls: calls.clone(),
            delay,
        }),
    )
    .unwrap();
    calls
}

/// A flow document with `steps` and `config`.
fn flow_json(id: &str, steps: &serde_json::Value, config: &serde_json::Value) -> String {
    json!({ "id": id, "name": id, "version": "0.0.1", "steps": steps, "config": config })
        .to_string()
}

fn flow_error(result: Result<(), RuntimeError>) -> String {
    match result {
        Err(RuntimeError::Flow(message)) => message,
        Err(other) => panic!("expected RuntimeError::Flow, got {other}"),
        Ok(()) => panic!("the flow must be refused"),
    }
}

// ---------------------------------------------------------------------------
// Load time
// ---------------------------------------------------------------------------

/// `"Stop"` is not `"stop"`: read as "continue", it would run the handler
/// after a failed auth step.
#[test]
fn an_on_error_other_than_stop_or_continue_is_refused() {
    let mut w = wafer();
    for on_error in ["Stop", "skip"] {
        let message = flow_error(w.add_flow_json(&flow_json(
            "f",
            &json!([{ "id": "a", "block": "x/y" }]),
            &json!({ "on_error": on_error }),
        )));
        assert!(message.contains("unknown variant"), "{on_error}: {message}");
    }
    assert!(w.flow_defs().is_empty());
}

/// A timeout the parser cannot read is not "no timeout".
#[test]
fn a_malformed_timeout_is_refused() {
    let mut w = wafer();
    for config in [
        json!({ "timeout": "30 seconds" }),
        json!({ "timeout": "0s" }),
        json!({ "timeout": "5124095576030432h" }),
        json!({ "timeout_ms": 0 }),
        json!({ "timeout": "30s", "timeout_ms": 30000 }),
    ] {
        let result = w.add_flow_json(&flow_json(
            "f",
            &json!([{ "id": "a", "block": "x/y" }]),
            &config,
        ));
        flow_error(result);
    }
    assert!(w.flow_defs().is_empty());
}

/// The executor routes `next` over top-level steps only.
#[test]
fn a_next_into_a_parallel_branch_is_refused() {
    let mut w = wafer();
    let message = flow_error(w.add_flow_json(&flow_json(
        "f",
        &json!([
            { "id": "fan", "block": "x/y", "parallel": [
                { "steps": [ { "id": "branch-step", "block": "x/y" } ] }
            ] },
            { "id": "route", "block": "x/y", "next": [ { "step": "branch-step" } ] }
        ]),
        &json!({}),
    )));
    assert!(
        message.contains("'branch-step', which is not a top-level step"),
        "{message}"
    );
}

/// The typed entry point validates too.
#[test]
fn a_transfer_to_the_flow_itself_is_refused() {
    let mut w = wafer();
    let flow: wafer_flow::WaferFlow = serde_json::from_str(&flow_json(
        "loop",
        &json!([{ "id": "a", "block": "x/y", "next": [ { "flow": "loop" } ] }]),
        &json!({}),
    ))
    .unwrap();
    let message = flow_error(w.add_flow(flow));
    assert!(
        message.contains("transfers to its own flow 'loop'"),
        "{message}"
    );
    assert!(w.flow_defs().is_empty());
}

// ---------------------------------------------------------------------------
// Run time
// ---------------------------------------------------------------------------

/// A flow timeout past the end of the clock is one no run reaches: the flow
/// runs instead of the deadline arithmetic overflowing.
#[tokio::test]
async fn a_timeout_past_the_end_of_the_clock_runs() {
    let mut w = wafer();
    let calls = counter(&mut w, "test/count", Duration::ZERO);
    w.add_flow_json(&flow_json(
        "f",
        &json!([{ "id": "a", "block": "test/count" }]),
        &json!({ "timeout": "5124095576030431h" }),
    ))
    .unwrap();
    w.seal().await.unwrap();

    let out = w
        .run("f", Message::new("http.request"), InputStream::empty())
        .await;
    assert!(out.collect_buffered().await.is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
