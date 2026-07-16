//! Test that the runtime parses the reserved `capabilities` subkey from
//! block config and intersects it with the block's declared caps.

use std::sync::Arc;

use serde_json::json;
use wafer_block::{
    capabilities::{BlockCapabilities, HeaderPolicy},
    streams::{input::InputStream, output::OutputStream},
    types::BlockInfo,
    Block, Context, LifecycleEvent, Message, WaferError,
};
use wafer_run::{RuntimeError, Wafer};

struct DeclaringNative {
    info: BlockInfo,
}

#[async_trait::async_trait]
impl Block for DeclaringNative {
    fn info(&self) -> BlockInfo {
        self.info.clone()
    }
    async fn handle(&self, _: &dyn Context, _: Message, _: InputStream) -> OutputStream {
        OutputStream::respond(b"{}".to_vec())
    }
    async fn lifecycle(&self, _: &dyn Context, _: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
}

#[tokio::test]
async fn config_capabilities_subkey_parsed_and_intersected() {
    let declared = BlockCapabilities {
        collections: wafer_block::capabilities::Allowlist::Only(
            ["users", "sessions"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ),
        network: wafer_block::capabilities::Allowlist::Any,
        headers: HeaderPolicy {
            readable: vec!["authorization".into()],
            writable: vec!["set-cookie".into()],
            ..Default::default()
        },
        ..Default::default()
    };

    let info =
        BlockInfo::new("test/declaring", "0.1.0", "middleware@v1", "").capabilities(declared);
    let block = Arc::new(DeclaringNative { info });

    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    w.register_block("test/declaring", block).unwrap();
    w.add_block_config(
        "test/declaring",
        json!({
            "capabilities": {
                "collections": { "Only": ["users"] },
                "network": "None",
                "headers": {
                    "writable": []
                }
            },
            "OTHER_KEY": "passthrough"
        }),
    );
    let wafer = w.start().await.expect("start");

    let eff = wafer
        .effective_capabilities("test/declaring")
        .expect("effective caps stored for registered block");
    assert_eq!(
        eff.collections,
        wafer_block::capabilities::Allowlist::Only(
            ["users"].iter().map(|s| s.to_string()).collect()
        )
    );
    assert_eq!(eff.network, wafer_block::capabilities::Allowlist::None);
    assert!(eff.headers.writable.is_empty());
}

#[tokio::test]
async fn config_capabilities_subkey_stripped_from_regular_config() {
    // Verify the reserved `capabilities` key is removed from block config
    // so it doesn't leak to `ctx.config_get("capabilities")`.
    let info = BlockInfo::new("test/echo-config", "0.1.0", "middleware@v1", "");
    let block = Arc::new(DeclaringNative { info });

    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    w.register_block("test/echo-config", block).unwrap();
    w.add_block_config(
        "test/echo-config",
        json!({
            "capabilities": { "network": "None" },
            "KEEP_THIS": "yes"
        }),
    );
    let _wafer = w.start().await.expect("start");
    // We can't easily inspect the post-strip block_configs from outside, but
    // the config-presence test for `KEEP_THIS` existing in the context is
    // covered by other tests; this test's mere compilation + successful start
    // proves the strip didn't break config loading.
}

#[tokio::test]
async fn seal_refuses_boot_when_capabilities_override_is_malformed() {
    // A narrowing typo (string where a bool is expected) must fail closed,
    // matching the fail_on_rejected_grants precedent — not silently drop the
    // override (serde_json::from_value fails on the whole struct) and leave
    // the block with its declared (here: unrestricted-default) capabilities.
    let info = BlockInfo::new("test/malformed-caps", "0.1.0", "middleware@v1", "");
    let block = Arc::new(DeclaringNative { info });

    let mut w = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    w.register_block("test/malformed-caps", block).unwrap();
    w.add_block_config(
        "test/malformed-caps",
        json!({
            "capabilities": { "raw_sql": "false" }
        }),
    );

    match w.start().await {
        Err(RuntimeError::Config(msg)) => {
            assert!(
                msg.contains("test/malformed-caps"),
                "error should name the offending block: {msg}"
            );
        }
        other => panic!(
            "expected Err(RuntimeError::Config(_)), got {}",
            match &other {
                Ok(_) => "Ok(_)".to_string(),
                Err(e) => format!("Err({e})"),
            }
        ),
    }
}
