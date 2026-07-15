//! End-to-end integration: declared capabilities are read from BlockInfo,
//! intersected with operator config, and applied at dispatch / sanitization.
//!
//! Uses wafer-test-support::WaferBuilder for runtime setup.

use std::sync::Arc;

use serde_json::json;
use wafer_block::{
    capabilities::BlockCapabilities,
    streams::{input::InputStream, output::OutputStream},
    types::{BlockInfo, BlockRuntime},
    Block, Context, LifecycleEvent, Message, WaferError,
};
use wafer_test_support::builder::WaferBuilder;

/// A native block that declares capabilities via BlockInfo::capabilities.
/// Native blocks do not enforce caps, but declarations are still stored
/// (visible via inspector / effective_capabilities).
struct DeclaringNative {
    info: BlockInfo,
}

#[async_trait::async_trait]
impl Block for DeclaringNative {
    fn info(&self) -> BlockInfo {
        self.info.clone()
    }
    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        // Echo the inbound meta key set into the response body as JSON.
        let keys: Vec<String> = msg.meta.iter().map(|e| e.key.clone()).collect();
        OutputStream::respond(serde_json::to_vec(&json!({"received_keys": keys})).unwrap())
    }
    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> Result<(), WaferError> {
        Ok(())
    }
}

fn make_declaring(name: &str, caps: BlockCapabilities) -> Arc<DeclaringNative> {
    let info = BlockInfo::new(name, "0.1.0", "middleware@v1", "declares caps").capabilities(caps);
    Arc::new(DeclaringNative { info })
}

#[tokio::test]
async fn declared_caps_applied_when_no_config() {
    let declared = BlockCapabilities {
        collections: ["users"].iter().map(|s| s.to_string()).collect(),
        crypto: true,
        ..Default::default()
    };
    let block = make_declaring("test/declaring-a", declared.clone());
    let wafer = WaferBuilder::new()
        .with_block("test/declaring-a", block)
        .build()
        .await
        .expect("build");

    let eff = wafer
        .effective_capabilities("test/declaring-a")
        .expect("effective caps stored");
    assert!(eff.crypto);
    assert!(eff.collections.contains("users"));
}

/// SEC-02: a WASM block that declares no capabilities must fail closed —
/// effective capabilities default to `none()`, not the trusted-native
/// `unrestricted()`. An omitted declaration on an untrusted guest must not
/// silently grant crypto, network, raw SQL, DDL, config, storage, vector,
/// and (critically) `call_block` to every registered block.
#[tokio::test]
async fn wasm_block_without_capabilities_defaults_to_none() {
    let info = BlockInfo::new("test/wasm-undeclared", "0.1.0", "middleware@v1", "no caps")
        .runtime(BlockRuntime::Wasm);
    let block = Arc::new(DeclaringNative { info });
    let wafer = WaferBuilder::new()
        .with_block("test/wasm-undeclared", block)
        .build()
        .await
        .expect("build");

    let eff = wafer
        .effective_capabilities("test/wasm-undeclared")
        .expect("effective caps stored");
    assert!(!eff.crypto, "crypto must be denied");
    assert!(!eff.network, "network must be denied");
    assert!(!eff.raw_sql, "raw_sql must be denied");
    assert!(!eff.ddl, "ddl must be denied");
    assert!(!eff.config, "config must be denied");
    assert!(eff.collections.is_empty(), "no collection wildcard");
    assert!(eff.storage_folders.is_empty(), "no storage wildcard");
    assert!(eff.vector_indexes.is_empty(), "no vector wildcard");
    assert!(
        eff.callable_blocks.is_empty(),
        "no callable-block wildcard — an undeclared WASM guest cannot call arbitrary blocks"
    );
}

/// A native block with no declared capabilities retains the historical
/// `unrestricted()` default — native blocks are compiled into the host
/// binary and trusted. This locks the runtime-type split introduced for
/// SEC-02 so the WASM fail-closed change does not regress native blocks.
#[tokio::test]
async fn native_block_without_capabilities_defaults_to_unrestricted() {
    // runtime defaults to BlockRuntime::Native.
    let info = BlockInfo::new(
        "test/native-undeclared",
        "0.1.0",
        "middleware@v1",
        "no caps",
    );
    let block = Arc::new(DeclaringNative { info });
    let wafer = WaferBuilder::new()
        .with_block("test/native-undeclared", block)
        .build()
        .await
        .expect("build");

    let eff = wafer
        .effective_capabilities("test/native-undeclared")
        .expect("effective caps stored");
    assert!(eff.crypto, "native retains unrestricted crypto");
    assert!(eff.network, "native retains unrestricted network");
    assert!(
        eff.callable_blocks.contains("*"),
        "native retains unrestricted call_block"
    );
}

#[tokio::test]
async fn operator_config_narrows_declared() {
    let declared = BlockCapabilities {
        network: true,
        network_allow: vec!["https://a.com/".into(), "https://b.com/".into()],
        ..Default::default()
    };
    let block = make_declaring("test/declaring-b", declared);
    let wafer = WaferBuilder::new()
        .with_block("test/declaring-b", block)
        .with_config(
            "test/declaring-b",
            json!({
                "capabilities": {
                    "network": true,
                    "network_allow": ["https://a.com/"]
                }
            }),
        )
        .build()
        .await
        .expect("build");

    let eff = wafer.effective_capabilities("test/declaring-b").unwrap();
    assert!(eff.network);
    assert_eq!(eff.network_allow, vec!["https://a.com/".to_string()]);
}

#[tokio::test]
async fn operator_config_cannot_widen_declared() {
    let declared = BlockCapabilities {
        network: false, // Explicitly denied.
        ..Default::default()
    };
    let block = make_declaring("test/declaring-c", declared);
    let wafer = WaferBuilder::new()
        .with_block("test/declaring-c", block)
        .with_config(
            "test/declaring-c",
            json!({
                "capabilities": {
                    "network": true   // Widening attempt.
                }
            }),
        )
        .build()
        .await
        .expect("build");

    let eff = wafer.effective_capabilities("test/declaring-c").unwrap();
    assert!(!eff.network, "narrower declaration wins");
}

#[tokio::test]
async fn native_block_declares_but_not_enforced() {
    let declared = BlockCapabilities {
        collections: ["users"].iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    let block = make_declaring("test/native-declared", declared);
    let wafer = WaferBuilder::new()
        .with_block("test/native-declared", block)
        .build()
        .await
        .expect("build");

    // Declaration is observable.
    let eff = wafer
        .effective_capabilities("test/native-declared")
        .expect("stored");
    assert!(eff.collections.contains("users"));

    // Dispatch succeeds regardless of declared collection restrictions (native is trusted).
    let msg = Message::new("http.request");
    let out = wafer
        .run_block("test/native-declared", msg, InputStream::empty())
        .await;
    match out.collect_buffered().await {
        Ok(buf) => {
            let resp: serde_json::Value = serde_json::from_slice(&buf.body).unwrap();
            assert!(resp.get("received_keys").is_some());
        }
        other => panic!("expected Respond from native block, got {other:?}"),
    }
}

#[tokio::test]
async fn config_capabilities_subkey_hidden_from_block_config_get() {
    // Verify the reserved "capabilities" key doesn't leak into ctx.config_get.
    struct ConfigInspector {
        info: BlockInfo,
    }

    #[async_trait::async_trait]
    impl Block for ConfigInspector {
        fn info(&self) -> BlockInfo {
            self.info.clone()
        }
        async fn handle(&self, ctx: &dyn Context, _: Message, _: InputStream) -> OutputStream {
            let caps = ctx
                .config_get("capabilities")
                .unwrap_or("MISSING")
                .to_string();
            let keep = ctx.config_get("KEEP_THIS").unwrap_or("MISSING").to_string();
            OutputStream::respond(
                serde_json::to_vec(&json!({"capabilities_subkey": caps, "KEEP_THIS": keep}))
                    .unwrap(),
            )
        }
        async fn lifecycle(&self, _: &dyn Context, _: LifecycleEvent) -> Result<(), WaferError> {
            Ok(())
        }
    }

    let info = BlockInfo::new(
        "test/inspector",
        "0.1.0",
        "middleware@v1",
        "inspects config",
    );
    let block = Arc::new(ConfigInspector { info });

    let wafer = WaferBuilder::new()
        .with_block("test/inspector", block)
        .with_config(
            "test/inspector",
            json!({
                "capabilities": {"network": false},
                "KEEP_THIS": "yes"
            }),
        )
        .build()
        .await
        .expect("build");

    let out = wafer
        .run_block(
            "test/inspector",
            Message::new("http.request"),
            InputStream::empty(),
        )
        .await;
    match out.collect_buffered().await {
        Ok(buf) => {
            let resp: serde_json::Value = serde_json::from_slice(&buf.body).unwrap();
            // The reserved subkey must be stripped from block config.
            assert_eq!(resp["capabilities_subkey"], "MISSING");
            // Other keys must pass through.
            assert_eq!(resp["KEEP_THIS"], "yes");
        }
        other => panic!("expected Respond, got {other:?}"),
    }
}
