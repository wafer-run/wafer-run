//! Every in-tree block that declares config, for the tests that walk all of
//! them: `empty_config_source_boots.rs` boots each over an empty source, and
//! `blockinfo_config_naming.rs` checks each declaration's key and label.
//!
//! Each list here is also what the tests walk, so the list cannot name a
//! block that is not walked:
//!
//! - [`linked_static_block_crates!`] writes, from one list of crate names,
//!   both the `use … as _;` line that links each crate (only that line puts
//!   its `linkme` entry into `STATIC_BLOCK_REGISTRATIONS`) and
//!   [`LINKED_STATIC_BLOCK_CRATES`]. A name without its crate does not
//!   compile.
//! - [`SERVICE_BLOCKS_WITH_CONFIG`] pairs each `wafer-core` service-block
//!   file with the constructor [`every_block`] builds it from.
//!
//! `empty_config_source_boots::every_block_that_declares_config_is_covered`
//! compares both lists with the source tree: it fails when a `wafer-block-*`
//! crate calls `register_static_block!` but is not linked here, when a
//! `wafer-core` service block declares config but has no pair here (or a
//! pair names a file that declares none), and when a pair's constructor
//! builds a block other than its file's. A new block is therefore walked by
//! both tests, or a test fails.

#![allow(dead_code, reason = "each test binary uses a different subset")]

use std::sync::Arc;

use wafer_block::{Block, STATIC_BLOCK_REGISTRATIONS};
use wafer_core::{
    interfaces::{
        database::service::DatabaseService,
        network::service::{NetworkError, NetworkService, Request, Response},
    },
    service_blocks::{database::DatabaseBlock, network::NetworkBlock},
};

/// Links each named crate with `use $krate as _;` and lists the same names,
/// as written (`wafer_block_cors`), in `LINKED_STATIC_BLOCK_CRATES`.
macro_rules! linked_static_block_crates {
    ($($krate:ident),+ $(,)?) => {
        $(use $krate as _;)+

        /// Every statically registered block crate this module links, by
        /// its Rust name (`wafer_block_cors` for `crates/wafer-block-cors`).
        pub const LINKED_STATIC_BLOCK_CRATES: &[&str] = &[$(stringify!($krate)),+];
    };
}

linked_static_block_crates!(
    wafer_block_cors,
    wafer_block_http_listener,
    wafer_block_inspector,
    wafer_block_ip_rate_limit,
    wafer_block_monitoring,
    wafer_block_postgres,
    wafer_block_readonly_guard,
    wafer_block_router,
    wafer_block_s3,
    wafer_block_security_headers,
    wafer_block_web,
);

/// A block with the name it registers under.
pub type NamedBlock = (&'static str, Arc<dyn Block>);

/// Builds one service block, for [`SERVICE_BLOCKS_WITH_CONFIG`].
pub type BuildBlock = fn() -> NamedBlock;

/// Each `wafer-core` service block that declares config: its file under
/// `wafer-core/src/service_blocks/`, and the constructor [`every_block`]
/// builds it from.
pub const SERVICE_BLOCKS_WITH_CONFIG: &[(&str, BuildBlock)] = &[
    ("database.rs", database_block),
    ("network.rs", network_block),
];

/// The database block over an in-memory SQLite database.
fn database_block() -> NamedBlock {
    let db: Arc<dyn DatabaseService> = Arc::new(
        wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory().expect("sqlite"),
    );
    (DatabaseBlock::NAME, Arc::new(DatabaseBlock::new(db)))
}

/// The network block over [`NoNetwork`].
fn network_block() -> NamedBlock {
    (
        NetworkBlock::NAME,
        Arc::new(NetworkBlock::new(Arc::new(NoNetwork))),
    )
}

/// A network service that refuses every request.
pub struct NoNetwork;

#[wafer_block::wafer_async_trait]
impl NetworkService for NoNetwork {
    async fn do_request(&self, _req: &Request) -> Result<Response, NetworkError> {
        Err(NetworkError::Other("no network in this test".into()))
    }
}

/// Every in-tree block that declares config, with the name it registers
/// under: each static registration, then each of
/// [`SERVICE_BLOCKS_WITH_CONFIG`].
pub fn every_block() -> Vec<NamedBlock> {
    STATIC_BLOCK_REGISTRATIONS
        .iter()
        .map(|entry| (entry.name, (entry.factory)()))
        .chain(SERVICE_BLOCKS_WITH_CONFIG.iter().map(|(_, build)| build()))
        .collect()
}
