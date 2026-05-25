//! Integration test for Wave 13 PR A: a block that emits `Halt` must
//! short-circuit the flow executor — downstream steps must NOT run.
//! This is the test that would have caught Wave 12 PR A's regression.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use wafer_block::streams::output::TerminalNotResponse;
use wafer_flow::{Step, WaferFlow};
use wafer_run::*;

// ---------------------------------------------------------------------------
// Helper: build an N-step flow
// ---------------------------------------------------------------------------

fn make_flow(id: &str, steps: Vec<Step>) -> WaferFlow {
    WaferFlow {
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

fn step(id: &str, block: &str) -> Step {
    Step {
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

// ---------------------------------------------------------------------------
// Block 1: emits Halt with body + meta — should short-circuit.
// ---------------------------------------------------------------------------

struct HaltBlock;

#[async_trait::async_trait]
impl Block for HaltBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/halt-sc", "0.0.1", "http-handler@v1", "halt fixture")
            .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::halt(
            b"halted".to_vec(),
            vec![
                MetaEntry {
                    key: "resp.status".into(),
                    value: "204".into(),
                },
                MetaEntry {
                    key: "resp.header.X-Halt-Test".into(),
                    value: "yes".into(),
                },
            ],
        )
    }
}

// ---------------------------------------------------------------------------
// Block 2: records whether it was invoked — must NOT be invoked.
// ---------------------------------------------------------------------------

struct AssertNotCalledBlock {
    called: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl Block for AssertNotCalledBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/assert-not-called-sc",
            "0.0.1",
            "http-handler@v1",
            "assertion fixture",
        )
        .instance_mode(InstanceMode::Singleton)
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        self.called.store(true, Ordering::SeqCst);
        OutputStream::respond(b"should not see this".to_vec())
    }
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn halt_short_circuits_flow_at_executor() {
    let called = Arc::new(AtomicBool::new(false));
    let assert_block = AssertNotCalledBlock {
        called: called.clone(),
    };

    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");

    w.register_block("test/halt-sc", Arc::new(HaltBlock))
        .unwrap();
    w.register_block("test/assert-not-called-sc", Arc::new(assert_block))
        .unwrap();

    // 2-step flow: halt-sc → assert-not-called-sc.
    w.add_flow(make_flow(
        "halt-then-assert",
        vec![
            step("halt-step", "test/halt-sc"),
            step("assert-step", "test/assert-not-called-sc"),
        ],
    ));

    w.seal().await.expect("seal failed");

    let output = w
        .run(
            "halt-then-assert",
            Message::new("http.request"),
            InputStream::empty(),
        )
        .await;

    // Assert: the assertion block was NOT called (proves short-circuit).
    assert!(
        !called.load(Ordering::SeqCst),
        "assertion block was invoked — Halt did not short-circuit the flow"
    );

    // Assert: the output stream carries Halt's body + meta.
    match output.collect_buffered().await {
        Err(TerminalNotResponse::Halt(resp)) => {
            assert_eq!(resp.body, b"halted", "Halt body mismatch");
            assert!(
                resp.meta
                    .iter()
                    .any(|m| m.key == "resp.status" && m.value == "204"),
                "missing resp.status=204 in Halt meta"
            );
            assert!(
                resp.meta
                    .iter()
                    .any(|m| m.key == "resp.header.X-Halt-Test" && m.value == "yes"),
                "missing resp.header.X-Halt-Test in Halt meta"
            );
        }
        other => panic!("expected Err(TerminalNotResponse::Halt), got {other:?}"),
    }
}
