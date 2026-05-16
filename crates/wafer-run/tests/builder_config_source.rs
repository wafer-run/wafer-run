//! Verifies that WaferBuilder::config_source plumbs the ConfigSource through
//! to the resulting Wafer, end-to-end.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §2

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use wafer_block::{
    core_types::{Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    Block, BlockInfo, ConfigVar, LifecycleEvent,
};
use wafer_run::{Context, StaticConfigSource, Wafer};

struct DeclaringBlock;
#[async_trait]
impl Block for DeclaringBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/declarer", "0.1.0", "test/iface@v1", "test")
            .config_keys(vec![ConfigVar::new("TEST__DECLARER__KEY", "doc", "")])
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
    async fn handle(&self, _ctx: &dyn Context, _m: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(Vec::new())
    }
}

#[tokio::test]
async fn builder_config_source_plumbs_through_to_validate() {
    // No value present → validate reports the block as broken.
    let empty_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut w_empty = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .config_source(empty_src)
        .build()
        .expect("build with empty source");
    w_empty
        .register_block("test/declarer", Arc::new(DeclaringBlock))
        .expect("register");
    let report_empty = w_empty.validate_all_block_configs().await;
    assert_eq!(
        report_empty.broken.len(),
        1,
        "expected broken: {report_empty:?}"
    );
    assert_eq!(report_empty.broken[0].block, "test/declarer");

    // Value present → validate reports ok.
    let mut data = HashMap::new();
    data.insert("TEST__DECLARER__KEY".to_string(), "value".to_string());
    let filled_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::new(data));
    let mut w_filled = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .config_source(filled_src)
        .build()
        .expect("build with filled source");
    w_filled
        .register_block("test/declarer", Arc::new(DeclaringBlock))
        .expect("register");
    let report_filled = w_filled.validate_all_block_configs().await;
    assert!(
        report_filled.broken.is_empty(),
        "expected ok: {report_filled:?}"
    );
    assert_eq!(report_filled.ok, vec!["test/declarer".to_string()]);
}
