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

/// A runtime whose only config is `source`, with nothing registered.
fn runtime(source: &[(&str, &str)]) -> Wafer {
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
    Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .config_source(Arc::new(StaticConfigSource::new(data)))
        .build()
        .expect("build")
}

mod network {
    use std::{sync::Mutex, time::Duration};

    use wafer_core::interfaces::network::service::{
        NetworkError, NetworkLimits, NetworkService, Request, Response, CONNECT_TIMEOUT_SECS_KEY,
        STREAM_TIMEOUT_SECS_KEY,
    };

    use super::*;

    const NETWORK: &str = "wafer-run/network";

    /// Records the limits the network block hands it.
    #[derive(Default)]
    struct RecordingNetwork {
        configured: Mutex<Vec<NetworkLimits>>,
    }

    #[async_trait::async_trait]
    impl NetworkService for RecordingNetwork {
        async fn do_request(&self, _req: &Request) -> Result<Response, NetworkError> {
            Err(NetworkError::Other("not used".into()))
        }

        fn configure(&self, limits: NetworkLimits) {
            self.configured.lock().unwrap().push(limits);
        }
    }

    /// The limits the ConfigSource holds reach the service at the block's
    /// Init; one it leaves unset keeps its default.
    #[tokio::test]
    async fn network_init_hands_the_config_source_limits_to_the_service() {
        let mut wafer = runtime(&[
            (CONNECT_TIMEOUT_SECS_KEY, "3"),
            (STREAM_TIMEOUT_SECS_KEY, "60"),
        ]);
        let service = Arc::new(RecordingNetwork::default());
        wafer_core::service_blocks::network::register_with(&mut wafer, service.clone())
            .expect("register");
        wafer.init_block(NETWORK).await.expect("Init");

        let configured = service.configured.lock().unwrap().clone();
        assert_eq!(
            configured,
            vec![NetworkLimits {
                connect_timeout: Duration::from_secs(3),
                stream_timeout: Some(Duration::from_secs(60)),
                ..NetworkLimits::default()
            }],
            "Init must configure the service once, with the ConfigSource's values"
        );
    }

    /// An invalid limit in the ConfigSource fails Init, naming the var.
    #[tokio::test]
    async fn an_invalid_network_limit_fails_init() {
        let mut wafer = runtime(&[(CONNECT_TIMEOUT_SECS_KEY, "soon")]);
        let service = Arc::new(RecordingNetwork::default());
        wafer_core::service_blocks::network::register_with(&mut wafer, service.clone())
            .expect("register");
        let message = permanent_message(wafer.init_block(NETWORK).await);
        assert!(message.contains(CONNECT_TIMEOUT_SECS_KEY), "{message}");
        assert!(service.configured.lock().unwrap().is_empty());
    }
}

mod database {
    use wafer_block_sqlite::service::SQLiteDatabaseService;
    use wafer_core::interfaces::database::{
        exec::DbExec, handler::STRICT_SCHEMA_CONFIG_KEY, service::DatabaseService,
    };

    use super::*;

    /// STRICT_SCHEMA set only in the ConfigSource reaches the backend at the
    /// database block's Init.
    #[tokio::test]
    async fn database_init_reads_strict_schema_from_the_config_source() {
        let mut wafer = runtime(&[(STRICT_SCHEMA_CONFIG_KEY, "true")]);
        let service = Arc::new(SQLiteDatabaseService::open_in_memory().expect("sqlite"));
        let as_service: Arc<dyn DatabaseService> = service.clone();
        wafer_core::service_blocks::database::register_with(&mut wafer, as_service)
            .expect("register");
        wafer.init_block("wafer-run/database").await.expect("Init");
        assert!(
            DbExec::strict_schema(service.as_ref()),
            "the ConfigSource's STRICT_SCHEMA must reach the backend"
        );
    }
}
