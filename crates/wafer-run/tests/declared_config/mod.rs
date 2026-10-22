//! Every in-tree block that declares config, for the tests that walk all of
//! them: `empty_config_source_boots.rs` boots each over an empty source, and
//! `blockinfo_config_naming.rs` checks each declaration's key and label.
//!
//! `empty_config_source_boots::every_block_that_declares_config_is_covered`
//! fails when a block crate that registers statically, or a `wafer-core`
//! service block that declares config, is missing from the lists below, so
//! a new block is walked by both tests or the build is red.

#![allow(dead_code, reason = "each test binary uses a different subset")]

use std::sync::Arc;

use wafer_block::{Block, STATIC_BLOCK_REGISTRATIONS};
// Link every statically registered block crate, so its `linkme` entry is in
// `STATIC_BLOCK_REGISTRATIONS` in the test binary that uses this module.
use wafer_block_cors as _;
use wafer_block_http_listener as _;
use wafer_block_inspector as _;
use wafer_block_ip_rate_limit as _;
use wafer_block_monitoring as _;
use wafer_block_postgres as _;
use wafer_block_readonly_guard as _;
use wafer_block_router as _;
use wafer_block_s3 as _;
use wafer_block_security_headers as _;
use wafer_block_web as _;
use wafer_core::{
    interfaces::{
        database::service::DatabaseService,
        network::service::{NetworkError, NetworkService, Request, Response},
    },
    service_blocks::{database::DatabaseBlock, network::NetworkBlock},
};

/// The crates whose `use` lines are above.
pub const LINKED_STATIC_BLOCK_CRATES: &[&str] = &[
    "wafer-block-cors",
    "wafer-block-http-listener",
    "wafer-block-inspector",
    "wafer-block-ip-rate-limit",
    "wafer-block-monitoring",
    "wafer-block-postgres",
    "wafer-block-readonly-guard",
    "wafer-block-router",
    "wafer-block-s3",
    "wafer-block-security-headers",
    "wafer-block-web",
];

/// The `wafer-core` service blocks that declare config, built by
/// [`every_block`].
pub const SERVICE_BLOCKS_WITH_CONFIG: &[&str] = &["database.rs", "network.rs"];

/// A network service that refuses every request.
pub struct NoNetwork;

#[wafer_block::wafer_async_trait]
impl NetworkService for NoNetwork {
    async fn do_request(&self, _req: &Request) -> Result<Response, NetworkError> {
        Err(NetworkError::Other("no network in this test".into()))
    }
}

/// Every in-tree block that declares config, with the name it registers
/// under: each static registration, then the `wafer-core` service blocks in
/// [`SERVICE_BLOCKS_WITH_CONFIG`], over an in-memory SQLite database and
/// [`NoNetwork`].
pub fn every_block() -> Vec<(&'static str, Arc<dyn Block>)> {
    let db: Arc<dyn DatabaseService> = Arc::new(
        wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory().expect("sqlite"),
    );
    STATIC_BLOCK_REGISTRATIONS
        .iter()
        .map(|entry| (entry.name, (entry.factory)()))
        .chain([
            (
                DatabaseBlock::NAME,
                Arc::new(DatabaseBlock::new(db)) as Arc<dyn Block>,
            ),
            (
                NetworkBlock::NAME,
                Arc::new(NetworkBlock::new(Arc::new(NoNetwork))),
            ),
        ])
        .collect()
}
