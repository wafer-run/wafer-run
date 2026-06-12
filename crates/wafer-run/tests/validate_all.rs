//! Wafer::validate_all_block_configs — declared-key existence check
//! used by the /_health route in PR 3.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §5

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    Block, BlockInfo, ConfigVar,
};
use wafer_run::{Context, StaticConfigSource, Wafer};

struct DeclaringBlock {
    name: &'static str,
    keys: Vec<ConfigVar>,
}

#[async_trait]
impl Block for DeclaringBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.1.0", "test/iface@v1", "test").config_keys(self.keys.clone())
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        panic!("validate must not invoke lifecycle");
    }
    async fn handle(&self, _ctx: &dyn Context, _m: Message, _input: InputStream) -> OutputStream {
        panic!("validate must not invoke handle");
    }
}

#[tokio::test]
async fn validate_reports_ok_when_all_required_present() {
    let mut data = HashMap::new();
    data.insert("TEST__A__REQ".to_string(), "v".to_string());
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::new(data));
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block(
            "test/a",
            Arc::new(DeclaringBlock {
                name: "test/a",
                keys: vec![ConfigVar::new("TEST__A__REQ", "doc", "")],
            }),
        )
        .unwrap();

    let report = wafer.validate_all_block_configs().await;
    assert!(report.broken.is_empty(), "got {:?}", report.broken);
    assert_eq!(report.ok, vec!["test/a".to_string()]);
}

/// Block whose `handle` runs `ctx.validate_all_block_configs()` (the
/// `Context`-trait surface) and parks the report for the test to inspect.
struct ProbeBlock {
    report: Arc<std::sync::Mutex<Option<wafer_run::ValidationReport>>>,
}

#[async_trait]
impl Block for ProbeBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new("test/probe", "0.1.0", "test/iface@v1", "test")
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
    async fn handle(&self, ctx: &dyn Context, _m: Message, _input: InputStream) -> OutputStream {
        let report = ctx.validate_all_block_configs().await;
        *self.report.lock().expect("report mutex") = Some(report);
        OutputStream::respond(Vec::new())
    }
}

/// The `Context`-trait surface must validate the canonical block view only:
/// an aliased block is reported once, under its registered name — the alias
/// key in `all_blocks` is skipped. (Wafer::validate_all_block_configs and the
/// context surface share one implementation; this pins the alias behavior.)
#[tokio::test]
async fn context_validate_skips_alias_entries() {
    let report_slot: Arc<std::sync::Mutex<Option<wafer_run::ValidationReport>>> =
        Arc::new(std::sync::Mutex::new(None));

    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block(
            "test/a",
            Arc::new(DeclaringBlock {
                name: "test/a",
                keys: vec![],
            }),
        )
        .unwrap();
    wafer
        .register_block(
            "test/probe",
            Arc::new(ProbeBlock {
                report: report_slot.clone(),
            }),
        )
        .unwrap();
    wafer.add_alias("@a", "test/a").expect("add_alias");
    // Populate all_blocks (registered blocks + the "@a" alias key).
    wafer.rebuild_all_blocks();
    let wafer = Arc::new(wafer);

    let out = wafer
        .run_block("test/probe", Message::new(""), InputStream::empty())
        .await;
    drop(out);

    let report = report_slot
        .lock()
        .expect("report mutex")
        .take()
        .expect("probe block must have run");
    assert!(report.broken.is_empty(), "got {:?}", report.broken);
    assert_eq!(
        report.ok,
        vec!["test/a".to_string(), "test/probe".to_string()],
        "alias '@a' must not appear as a separate report entry"
    );
}

#[tokio::test]
async fn validate_reports_broken_for_missing_required() {
    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block(
            "test/a",
            Arc::new(DeclaringBlock {
                name: "test/a",
                keys: vec![ConfigVar::new("TEST__A__REQ", "doc", "")],
            }),
        )
        .unwrap();
    wafer
        .register_block(
            "test/b",
            Arc::new(DeclaringBlock {
                name: "test/b",
                keys: vec![],
            }),
        )
        .unwrap();

    let report = wafer.validate_all_block_configs().await;
    assert_eq!(report.ok, vec!["test/b".to_string()]);
    assert_eq!(report.broken.len(), 1);
    assert_eq!(report.broken[0].block, "test/a");
    assert_eq!(
        report.broken[0].missing_keys,
        vec!["TEST__A__REQ".to_string()]
    );
}
