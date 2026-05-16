//! Verifies that lazy-init merges `Wafer::add_block_config` JSON into the
//! `lifecycle(Init).data`, alongside `ConfigSource`-resolved env keys.
//!
//! Regression: PR #98 dropped the `block_configs` feed into Init data,
//! breaking downstream blocks like `wafer-run/router` that consume their
//! config via `BlockConfig::from_event`.

use std::sync::Arc;

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, LifecycleType, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    Block, BlockInfo, ConfigVar,
};
use wafer_run::{StaticConfigSource, Wafer};

struct ConfigCaptureBlock {
    name: &'static str,
    captured: Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    declared: Vec<ConfigVar>,
}

#[async_trait]
impl Block for ConfigCaptureBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.1.0", "test/iface@v1", "test")
            .config_keys(self.declared.clone())
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        event: LifecycleEvent,
    ) -> Result<(), WaferError> {
        if event.event_type == LifecycleType::Init {
            let parsed: serde_json::Value =
                serde_json::from_slice(&event.data).unwrap_or(serde_json::Value::Null);
            *self.captured.lock().unwrap() = Some(parsed);
        }
        Ok(())
    }

    async fn handle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        OutputStream::respond(Vec::new())
    }
}

#[tokio::test]
async fn init_data_merges_block_configs_with_env_config() {
    let mut env = std::collections::HashMap::new();
    env.insert("TEST__MERGE__ENV_KEY".to_string(), "from-env".to_string());

    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::new(env));
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");

    let captured = Arc::new(std::sync::Mutex::new(None));
    let block = Arc::new(ConfigCaptureBlock {
        name: "test/merge",
        captured: captured.clone(),
        declared: vec![ConfigVar::new("TEST__MERGE__ENV_KEY", "doc", "")],
    });
    wafer.register_block("test/merge", block).expect("register");

    // Register arbitrary JSON via add_block_config. This is the "routes"-shaped
    // config path that PR #98 broke.
    wafer.add_block_config(
        "test/merge",
        serde_json::json!({
            "routes": [{"path": "/", "block": "x"}],
            "TEST__MERGE__ENV_KEY": "from-json", // env should win
        }),
    );
    wafer.seal().await.expect("seal");

    // Trigger init via run_block (top-level dispatch runs lazy init).
    let _out = Arc::new(wafer)
        .run_block(
            "test/merge",
            Message::new("test.init"),
            InputStream::empty(),
        )
        .await;

    let cap = captured
        .lock()
        .unwrap()
        .clone()
        .expect("init must have captured");
    let obj = cap.as_object().expect("init data must be a JSON object");
    assert_eq!(
        obj.get("routes"),
        Some(&serde_json::json!([{"path": "/", "block": "x"}])),
        "block_configs JSON must be present in init data"
    );
    assert_eq!(
        obj.get("TEST__MERGE__ENV_KEY"),
        Some(&serde_json::Value::String("from-env".into())),
        "env-config must override JSON config for the same key"
    );
}

#[tokio::test]
async fn init_data_passes_through_block_configs_when_no_declared_keys() {
    // A block with no declared ConfigVars (like wafer-run/router) should still
    // get its add_block_config JSON in event.data.
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");

    let captured = Arc::new(std::sync::Mutex::new(None));
    let block = Arc::new(ConfigCaptureBlock {
        name: "test/router-like",
        captured: captured.clone(),
        declared: vec![],
    });
    wafer
        .register_block("test/router-like", block)
        .expect("register");

    wafer.add_block_config(
        "test/router-like",
        serde_json::json!({"routes": [{"path": "/foo"}]}),
    );
    wafer.seal().await.expect("seal");

    let _out = Arc::new(wafer)
        .run_block(
            "test/router-like",
            Message::new("test.init"),
            InputStream::empty(),
        )
        .await;

    let cap = captured
        .lock()
        .unwrap()
        .clone()
        .expect("init must have captured");
    assert_eq!(
        cap.get("routes"),
        Some(&serde_json::json!([{"path": "/foo"}])),
    );
}
