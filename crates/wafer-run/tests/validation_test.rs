//! Integration tests for config validation and interface action validation.
//!
//! Test A: validate_all_block_configs reports missing required config.
//! Test B: validate_all_block_configs returns OK when all required config
//!         is provided via ConfigSource.
//! Test C: call_block rejects an action not in a known interface.
//! Test D: call_block allows an unknown (custom) interface with a warning.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use wafer_block::types::ConfigVar;
use wafer_run::{
    block::{Block, BlockInfo},
    context::Context,
    runtime::config_source::{ConfigSource, StaticConfigSource},
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
async fn validate_reports_missing_required_config() {
    // Lazy-init: missing required config no longer fails at `seal()` —
    // it surfaces on first dispatch. Operators check
    // `validate_all_block_configs()` proactively (e.g. on `/_health`).
    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    w.register_block("test-org/needs-config", Arc::new(NeedsConfigBlock))
        .unwrap();
    // No config provided => required key is absent.

    // `seal()` itself does NOT fail on missing required config.
    w.seal()
        .await
        .expect("seal must not validate config eagerly");

    // The validation report should flag the block as broken.
    let report = w.validate_all_block_configs().await;
    assert!(
        report
            .broken
            .iter()
            .any(|b| b.block == "test-org/needs-config"
                && b.missing_keys
                    .iter()
                    .any(|k| k == "TEST_ORG__NEEDS_CONFIG__API_KEY")),
        "expected broken report for test-org/needs-config / TEST_ORG__NEEDS_CONFIG__API_KEY, \
         got: {:?}",
        report.broken
    );
}

#[tokio::test]
async fn validate_succeeds_when_all_required_present() {
    // Required config is provided via the ConfigSource (the lazy-init
    // pathway). `add_block_config` is reserved for flow-event JSON, not
    // env-var declared keys.
    let mut data = HashMap::new();
    data.insert(
        "TEST_ORG__NEEDS_CONFIG__API_KEY".to_string(),
        "secret-value".to_string(),
    );
    let cfg_src: Arc<dyn ConfigSource> = Arc::new(StaticConfigSource::new(data));

    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .config_source(cfg_src)
        .build()
        .expect("empty wafer build is infallible");
    w.register_block("test-org/needs-config", Arc::new(NeedsConfigBlock))
        .unwrap();

    w.seal().await.expect("seal should succeed");
    let report = w.validate_all_block_configs().await;
    assert!(
        report.broken.is_empty(),
        "expected no broken blocks, got: {:?}",
        report.broken
    );
    assert!(
        report.ok.iter().any(|n| n == "test-org/needs-config"),
        "expected test-org/needs-config in ok list, got: {:?}",
        report.ok
    );
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
    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    w.register_block("test-org/db-block", Arc::new(DbBlock))
        .unwrap();
    w.register_block("test-org/bad-action-caller", Arc::new(BadActionCallerBlock))
        .unwrap();

    w.seal().await.expect("start should succeed");

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
    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    w.register_block("test-org/custom-iface", Arc::new(CustomInterfaceBlock))
        .unwrap();
    w.register_block(
        "test-org/custom-iface-caller",
        Arc::new(CustomIfaceCallerBlock),
    )
    .unwrap();

    w.seal().await.expect("start should succeed");

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
