//! Every in-tree block starts from a `ConfigSource` that holds nothing,
//! unless one of its keys is genuinely required.
//!
//! A declared `ConfigVar` with an empty default and no `.optional()` is
//! required (`resolve_declared`), and a required key the source lacks fails
//! the block's Init permanently. That is right for a key the block cannot run
//! without, and wrong for one whose absence means "off":
//! `WAFER_RUN__NETWORK__STREAM_TIMEOUT_SECS` was declared that way, so every
//! embedder that did not set it lost `wafer-run/network` at start. This test
//! reads what each block actually declares, so a key added the same way fails
//! here rather than in an embedder's boot.

use std::{collections::BTreeSet, path::Path, sync::Arc};

use wafer_block::STATIC_BLOCK_REGISTRATIONS;
// Link every statically registered block crate; `static_block_crates_are_all_linked`
// fails when one is missing from this list.
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
use wafer_core::interfaces::network::service::{NetworkError, NetworkService, Request, Response};
use wafer_run::{StaticConfigSource, Wafer};

/// The crates whose `use` lines are above.
const LINKED_STATIC_BLOCK_CRATES: &[&str] = &[
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

/// The `wafer-core` service blocks that declare config, registered below.
const SERVICE_BLOCKS_WITH_CONFIG: &[&str] = &["database.rs", "network.rs"];

/// The keys a block cannot run without: an empty source must fail on them.
const GENUINELY_REQUIRED: &[(&str, &str)] =
    &[("wafer-run/postgres", "WAFER_RUN__POSTGRES__DATABASE_URL")];

struct NoNetwork;

#[wafer_block::wafer_async_trait]
impl NetworkService for NoNetwork {
    async fn do_request(&self, _req: &Request) -> Result<Response, NetworkError> {
        Err(NetworkError::Other("no network in this test".into()))
    }
}

/// Every in-tree block that declares config, over a source holding nothing.
fn every_block_over_an_empty_source() -> Wafer {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .config_source(Arc::new(StaticConfigSource::default()))
        .build()
        .expect("build");
    for entry in STATIC_BLOCK_REGISTRATIONS.iter() {
        wafer
            .register_block(entry.name, (entry.factory)())
            .expect("register a static block");
    }
    let db: Arc<dyn wafer_core::interfaces::database::service::DatabaseService> = Arc::new(
        wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory().expect("sqlite"),
    );
    wafer_core::service_blocks::database::register_with(&mut wafer, db).expect("database");
    wafer_core::service_blocks::network::register_with(&mut wafer, Arc::new(NoNetwork))
        .expect("network");
    wafer
}

#[tokio::test]
async fn an_empty_source_satisfies_every_block_but_a_genuinely_required_key() {
    let wafer = every_block_over_an_empty_source();
    let report = wafer.validate_all_block_configs().await;
    let broken: BTreeSet<(String, String)> = report
        .broken
        .iter()
        .flat_map(|b| {
            b.missing_keys
                .iter()
                .map(move |key| (b.block.clone(), key.clone()))
        })
        .collect();
    let expected: BTreeSet<(String, String)> = GENUINELY_REQUIRED
        .iter()
        .map(|(block, key)| ((*block).to_string(), (*key).to_string()))
        .collect();
    assert_eq!(
        broken, expected,
        "a key whose absence means \"off\" must be `.optional()` (or carry a \
         non-empty default); only a key the block cannot run without may be \
         required"
    );
}

/// The network block starts with the stream total unset, and a set value
/// still reaches its Init.
#[tokio::test]
async fn the_network_block_starts_with_the_stream_timeout_unset_or_set() {
    let wafer = every_block_over_an_empty_source();
    wafer
        .init_block("wafer-run/network")
        .await
        .expect("network Init with the stream timeout unset");

    let info = wafer_block::Block::info(&wafer_core::service_blocks::network::NetworkBlock::new(
        Arc::new(NoNetwork),
    ))
    .config_keys;
    let source = StaticConfigSource::new(
        [(
            "WAFER_RUN__NETWORK__STREAM_TIMEOUT_SECS".to_string(),
            "6".to_string(),
        )]
        .into(),
    );
    let loaded = wafer_run::ConfigSource::load_for_block(&source, "wafer-run/network", &info)
        .await
        .expect("load");
    assert_eq!(
        loaded.get("WAFER_RUN__NETWORK__STREAM_TIMEOUT_SECS"),
        Some("6")
    );
}

/// The crates that register a block statically are exactly the ones linked
/// above, and the service blocks that declare config are the ones registered;
/// a new one fails here until it is covered.
#[test]
fn every_block_that_declares_config_is_covered() {
    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut static_crates = BTreeSet::new();
    for entry in std::fs::read_dir(&crates_dir).expect("read crates/") {
        let dir = entry.expect("dir entry").path();
        let name = dir.file_name().expect("name").to_string_lossy().to_string();
        let Ok(lib) = std::fs::read_to_string(dir.join("src/lib.rs")) else {
            continue;
        };
        if name.starts_with("wafer-block-")
            && (lib.contains("register_static_block!") || lib.contains("lazy block:"))
            && name != "wafer-block-macro"
        {
            static_crates.insert(name);
        }
    }
    let linked: BTreeSet<String> = LINKED_STATIC_BLOCK_CRATES
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(static_crates, linked);

    let service_dir = crates_dir.join("wafer-core/src/service_blocks");
    let mut with_config = BTreeSet::new();
    for entry in std::fs::read_dir(&service_dir).expect("read service_blocks/") {
        let path = entry.expect("dir entry").path();
        let source = std::fs::read_to_string(&path).expect("read");
        // Code, not the macro's documentation of the builder call.
        let declares = source
            .lines()
            .any(|line| line.contains(".config_keys(") && !line.trim_start().starts_with("//"));
        if declares {
            with_config.insert(
                path.file_name()
                    .expect("name")
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }
    let covered: BTreeSet<String> = SERVICE_BLOCKS_WITH_CONFIG
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(with_config, covered);
}
