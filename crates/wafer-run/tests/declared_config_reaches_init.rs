//! A block's declared `config_keys` are resolved through the embedder's
//! `ConfigSource` and reach the block in its Init payload. These drive the
//! real runtime (`Wafer::init_block`) with a `StaticConfigSource` and the
//! process environment untouched, so a block that reads its keys from
//! `std::env` instead sees nothing.

use std::{collections::HashMap, sync::Arc};

use wafer_block::STATIC_BLOCK_REGISTRATIONS;
// Link the block crates so their static registrations are in this binary.
use wafer_block_postgres as _;
use wafer_block_s3 as _;
use wafer_run::{InitError, StaticConfigSource, Wafer};

const POSTGRES: &str = "wafer-run/postgres";
const POSTGRES_URL: &str = "WAFER_RUN__POSTGRES__DATABASE_URL";
const S3: &str = "wafer-run/s3";
const S3_ENDPOINT: &str = "WAFER_RUN__S3__ENDPOINT";
const S3_MAX_OBJECT_BYTES: &str = "WAFER_RUN__S3__MAX_OBJECT_BYTES";

/// A runtime whose only config is `source`, with the static block `block`
/// registered.
fn wafer_with(source: &[(&str, &str)], block: &str) -> Wafer {
    let instance = STATIC_BLOCK_REGISTRATIONS
        .iter()
        .find(|entry| entry.name == block)
        .map_or_else(
            || panic!("{block} is not statically registered"),
            |entry| (entry.factory)(),
        );
    for (key, _) in source {
        assert!(
            std::env::var_os(key).is_none(),
            "{key} is set in the process env; this test proves the ConfigSource path"
        );
    }
    let data: HashMap<String, String> = source
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .config_source(Arc::new(StaticConfigSource::new(data)))
        .build()
        .expect("build");
    wafer.register_block(block, instance).expect("register");
    wafer
}

fn permanent_message(outcome: Result<wafer_run::InitializedState, InitError>) -> String {
    match outcome {
        Err(InitError::Permanent(message)) => message,
        other => panic!("expected a permanent Init failure, got {other:?}"),
    }
}

/// The URL is refused by the connect step itself (it turns the statement
/// cache off), so reaching that refusal proves Init read the URL the
/// ConfigSource holds; without it Init fails naming the var instead.
#[tokio::test]
async fn postgres_init_reads_its_url_from_the_config_source() {
    let wafer = wafer_with(
        &[(
            POSTGRES_URL,
            "postgres://u:p@127.0.0.1:1/db?statement-cache-capacity=0",
        )],
        POSTGRES,
    );
    let message = permanent_message(wafer.init_block(POSTGRES).await);
    assert!(message.contains("statement cache"), "{message}");
}

/// An invalid read cap in the ConfigSource fails Init, naming the var.
#[tokio::test]
async fn s3_init_reads_its_read_cap_from_the_config_source() {
    let wafer = wafer_with(
        &[
            (S3_ENDPOINT, "http://127.0.0.1:1"),
            (S3_MAX_OBJECT_BYTES, "0"),
        ],
        S3,
    );
    let message = permanent_message(wafer.init_block(S3).await);
    assert!(message.contains(S3_MAX_OBJECT_BYTES), "{message}");
}

/// An empty endpoint means AWS itself, so the endpoint is optional: a
/// source without one leaves the block's config complete.
#[tokio::test]
async fn s3_endpoint_is_optional() {
    let wafer = wafer_with(&[], S3);
    let report = wafer.validate_all_block_configs().await;
    assert!(report.broken.is_empty(), "{report:?}");
    assert_eq!(report.ok, vec![S3.to_string()]);
}
