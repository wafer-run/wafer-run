//! Integration tests for config validation and interface action validation.
//!
//! Test A: start() rejects missing required config.
//! Test B: start() succeeds when all required config is provided.
//! Test C: call_block rejects an action not in a known interface.
//! Test D: call_block allows an unknown (custom) interface with a warning.

use std::sync::Arc;

use async_trait::async_trait;
use wafer_block::types::ConfigVar;
use wafer_run::{
    block::{Block, BlockInfo},
    context::Context,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::{ErrorCode, InstanceMode, Message},
    Wafer,
};

// ---------------------------------------------------------------------------
// Test A & B — block with a required ConfigVar
// ---------------------------------------------------------------------------

// Block name: "test-org/needs-config"
// Expected prefix for config vars: "TEST_ORG__NEEDS_CONFIG__"
struct NeedsConfigBlock;

#[async_trait]
impl Block for NeedsConfigBlock {
    fn info(&self) -> BlockInfo {
        let mut info = BlockInfo::new(
            "test-org/needs-config",
            "0.1.0",
            "service@v1",
            "NeedsConfig",
        );
        // required: default is empty, auto_generate is false
        info.config_keys = vec![ConfigVar::new(
            "TEST_ORG__NEEDS_CONFIG__API_KEY",
            "Required API key",
            "", // empty default => required
        )];
        info
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(b"ok".to_vec())
    }
}

#[tokio::test]
async fn start_fails_on_missing_required_config() {
    let mut w = Wafer::new();
    w.register_block("test-org/needs-config", Arc::new(NeedsConfigBlock))
        .unwrap();
    // No add_block_config call => required key is absent

    let err = w.start_without_bind().await.unwrap_err();
    let err_msg = err.to_string();

    // The error should be a Config variant and mention the block name + key
    assert!(
        err_msg.contains("config error"),
        "expected 'config error' prefix, got: {err_msg}"
    );
    assert!(
        err_msg.contains("test-org/needs-config"),
        "error should mention block name, got: {err_msg}"
    );
    assert!(
        err_msg.contains("TEST_ORG__NEEDS_CONFIG__API_KEY"),
        "error should mention the missing key, got: {err_msg}"
    );
}

#[tokio::test]
async fn start_succeeds_when_all_required_present() {
    let mut w = Wafer::new();
    w.register_block("test-org/needs-config", Arc::new(NeedsConfigBlock))
        .unwrap();
    // Provide the required key
    w.add_block_config(
        "test-org/needs-config",
        serde_json::json!({ "TEST_ORG__NEEDS_CONFIG__API_KEY": "secret-value" }),
    );

    w.start_without_bind()
        .await
        .expect("start should succeed when all required config is provided");
}

// ---------------------------------------------------------------------------
// Test C — call_block rejects wrong action for known interface
//
// Setup: a "caller" block calls ctx.call_block("test-org/db-block", msg, input)
// where msg carries action "publish", which is not in database@v1.
// ---------------------------------------------------------------------------

struct DbBlock;

#[async_trait]
impl Block for DbBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test-org/db-block", "0.1.0", "database@v1", "DbBlock")
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        // This should never be reached in Test C — the validator should reject
        // the action before handle is called.
        OutputStream::respond(b"db-response".to_vec())
    }
}

/// A caller block that proxies a message with "publish" action to the db block.
struct BadActionCallerBlock;

#[async_trait]
impl Block for BadActionCallerBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test-org/bad-action-caller",
            "0.1.0",
            "service@v1",
            "BadActionCaller",
        )
        .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        // Call the database block with an action NOT in database@v1
        let mut bad_msg = Message::new("publish"); // "publish" is not a database@v1 action
        bad_msg.set_meta("req.action", "publish");
        ctx.call_block("test-org/db-block", bad_msg, input).await
    }
}

#[tokio::test]
async fn call_block_rejects_wrong_action_for_interface() {
    let mut w = Wafer::new();
    w.register_block("test-org/db-block", Arc::new(DbBlock))
        .unwrap();
    w.register_block("test-org/bad-action-caller", Arc::new(BadActionCallerBlock))
        .unwrap();

    w.start_without_bind().await.expect("start should succeed");

    // Run the caller block, which internally calls the db block with "publish"
    let output = w
        .run_block(
            "test-org/bad-action-caller",
            Message::new("trigger"),
            InputStream::empty(),
        )
        .await;

    match output.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => {
            assert_eq!(
                e.code,
                ErrorCode::InvalidArgument,
                "expected InvalidArgument, got: {:?}",
                e.code
            );
            // Error should mention the block name and/or the action
            let msg_lower = e.message.to_lowercase();
            assert!(
                msg_lower.contains("test-org/db-block") || msg_lower.contains("publish"),
                "error message should mention block name or action, got: {}",
                e.message
            );
        }
        other => panic!("expected Error(InvalidArgument) from bad-action call, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test D — call_block allows custom unknown interface (warn-once, no reject)
// ---------------------------------------------------------------------------

struct CustomInterfaceBlock;

#[async_trait]
impl Block for CustomInterfaceBlock {
    fn info(&self) -> BlockInfo {
        // "my-org/custom@v1" is not in wafer_block::interfaces::all()
        BlockInfo::new(
            "test-org/custom-iface",
            "0.1.0",
            "my-org/custom@v1",
            "CustomIface",
        )
        .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(b"custom-ok".to_vec())
    }
}

/// A caller block that calls the custom-interface block with any action.
struct CustomIfaceCallerBlock;

#[async_trait]
impl Block for CustomIfaceCallerBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test-org/custom-iface-caller",
            "0.1.0",
            "service@v1",
            "CustomIfaceCaller",
        )
        .instance_mode(InstanceMode::Singleton)
    }
    async fn handle(&self, ctx: &dyn Context, _msg: Message, input: InputStream) -> OutputStream {
        let mut fwd = Message::new("any-action");
        fwd.set_meta("req.action", "any-action");
        ctx.call_block("test-org/custom-iface", fwd, input).await
    }
}

#[tokio::test]
async fn call_block_allows_custom_interface_with_warning() {
    let mut w = Wafer::new();
    w.register_block("test-org/custom-iface", Arc::new(CustomInterfaceBlock))
        .unwrap();
    w.register_block(
        "test-org/custom-iface-caller",
        Arc::new(CustomIfaceCallerBlock),
    )
    .unwrap();

    w.start_without_bind().await.expect("start should succeed");

    // The caller block forwards to the custom-interface block.
    // Even though "my-org/custom@v1" is unknown, the call must NOT be rejected —
    // the runtime logs a warn-once and proceeds.
    let output = w
        .run_block(
            "test-org/custom-iface-caller",
            Message::new("trigger"),
            InputStream::empty(),
        )
        .await;

    match output.collect_buffered().await {
        Ok(buf) => {
            assert_eq!(
                buf.body, b"custom-ok",
                "expected 'custom-ok' response from custom-interface block"
            );
        }
        Err(other) => {
            panic!("expected successful response from custom-interface block, got: {other:?}")
        }
    }
}
