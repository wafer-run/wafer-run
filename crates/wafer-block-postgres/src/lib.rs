//! PostgreSQL database block — `wafer-run/postgres`.
//!
//! Self-contained block wrapping the PostgreSQL database service.
//! Uses the shared database message handler for the `database@v1` interface.

#![warn(missing_docs)]

mod errors;
mod params;
/// PostgreSQL implementation of `wafer_core::interfaces::database::service::DatabaseService`.
///
/// Exposed publicly so native consumers (e.g. a native application build) can construct
/// the service directly from a connection URL when running outside the
/// block lifecycle.
pub mod service;

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use service::PostgresDatabaseService;
use wafer_block::{BlockConfig, ConfigVar, ErrorCode, InputType, LifecycleType, WaferError};
use wafer_core::interfaces::database::{handler, service::DatabaseService};
use wafer_schema::{
    manifest::{collections_to_tables, CollectionDef},
    Table,
};

const DATABASE_URL_ENV: &str = "WAFER_RUN__POSTGRES__DATABASE_URL";

wafer_core::service_block! {
    /// The PostgreSQL database block.
    ///
    /// Initialized during `lifecycle(Init)`. Reads its connection URL from its
    /// declared `WAFER_RUN__POSTGRES__DATABASE_URL` config var, which the
    /// runtime resolves through the embedder's `ConfigSource` (a process
    /// environment, a settings table) and hands over in the Init payload — a
    /// wafer-run process typically points at one database, so this lives in
    /// `config_keys`.
    lazy block: pub(crate) PostgresDatabaseBlock,
    name: "wafer-run/postgres",
    version: "0.0.1",
    interface: "database@v1",
    description: "PostgreSQL database block",
    category: Infrastructure,
    service: dyn DatabaseService,
    extra_fields: { tables: OnceLock<Vec<Table>> },
    // NB: STRICT_SCHEMA (`WAFER_RUN__DATABASE__STRICT_SCHEMA`) is an
    // interface-level flag shared by every `database@v1` backend, so it is
    // declared once on the generic `wafer-run/database` block — the builder's
    // config-prefix check requires a block's declared keys to match its own
    // `WAFER_RUN__POSTGRES__` prefix, which the DATABASE-scoped key doesn't.
    // This block still *reads* the value via `ctx.config_get` at Init
    // (see `handle_lifecycle`); it simply doesn't re-declare it.
    info_extras: |_this, info| info.config_keys(vec![ConfigVar::new(
        DATABASE_URL_ENV,
        "PostgreSQL connection URL (postgres://user:pass@host:port/db). \
         Required.",
        "",
    )
    .name("Database URL")
    // A URL carries `user:password@`, so it is masked wherever config is
    // served back (admin API responses).
    .input_type(InputType::Password)]),
    handle: |service, _this, ctx, msg, body| {
        handler::handle_message(service.as_ref(), ctx, &msg, &body).await
    },
    lifecycle: |this, ctx, event| {
        if event.event_type == LifecycleType::Init && this.service.get().is_none() {
            let config = BlockConfig::from_event(&event);

            let tables = match config.get("collections") {
                Some(v) => {
                    let colls = serde_json::from_value::<HashMap<String, CollectionDef>>(
                        v.clone(),
                    )
                    .map_err(|e| {
                        WaferError::new(
                            ErrorCode::FailedPrecondition,
                            format!("wafer-run/postgres: invalid collections config: {e}"),
                        )
                    })?;
                    collections_to_tables(&colls).map_err(|e| {
                        WaferError::new(
                            ErrorCode::FailedPrecondition,
                            format!("wafer-run/postgres: invalid collections config: {e}"),
                        )
                    })?
                }
                None => Vec::new(),
            };
            this.tables.set(tables).ok();

            let url = match config.str(DATABASE_URL_ENV) {
                "" => {
                    return Err(WaferError::new(
                        ErrorCode::FailedPrecondition,
                        format!("wafer-run/postgres: {DATABASE_URL_ENV} must be set"),
                    ));
                }
                url => url,
            };

            // A server that is down or still starting keeps its
            // `Unavailable` code, so the runtime retries this Init.
            let svc = PostgresDatabaseService::connect(url)
                .await
                .map_err(|e| WaferError::new(e.code(), format!("wafer-run/postgres: {e}")))?;
            tracing::info!("PostgreSQL database connected");
            this.service.set(Arc::new(svc)).ok();
        }

        // Run table migrations on Init
        if event.event_type == LifecycleType::Init {
            let tables = this.tables.get().map_or(&[][..], |t| t.as_slice());
            if let Some(service) = this.service.get() {
                handler::handle_lifecycle(service.as_ref(), tables, ctx, &event).await?;
            }
        }

        Ok(())
    },
}

wafer_block::register_static_block!("wafer-run/postgres", PostgresDatabaseBlock);

#[cfg(test)]
mod tests {
    use wafer_block::{
        Block, Context, ErrorCode, InputStream, LifecycleEvent, LifecycleType, Message,
        OutputStream, TerminalNotResponse,
    };

    use super::{PostgresDatabaseBlock, DATABASE_URL_ENV};

    /// Minimal `Context` that panics if the block reaches back into the
    /// runtime, and serves no config.
    #[derive(Clone, Default)]
    struct NoopContext;

    #[async_trait::async_trait]
    impl Context for NoopContext {
        async fn call_block(
            &self,
            block_name: &str,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            panic!("NoopContext::call_block called unexpectedly: block_name={block_name}");
        }

        fn is_cancelled(&self) -> bool {
            false
        }

        fn config_get(&self, _key: &str) -> Option<&str> {
            None
        }

        fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
            std::sync::Arc::new(self.clone())
        }
        // Denies every access, as the trait's default `check_resource_access` does.
        fn resource_access_admitted(
            &self,
            _resource: &str,
            _resource_type: wafer_block::types::ResourceType,
            _access: wafer_block::types::ResourceAccess,
        ) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn handle_before_init_yields_typed_error_not_panic() {
        let block = PostgresDatabaseBlock::new();
        let out = block
            .handle(&NoopContext, Message::new("db.get"), InputStream::empty())
            .await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::Internal);
                assert!(
                    e.message.contains("wafer-run/postgres")
                        && e.message.contains("not initialized"),
                    "unexpected pre-Init error message: {}",
                    e.message
                );
            }
            other => panic!("expected typed pre-Init error terminal, got: {other:?}"),
        }
    }

    /// A malformed `collections` config value (a string, not the expected
    /// `HashMap<String, CollectionDef>` object) must not be swallowed and
    /// replaced with an empty table list — that would boot the block with
    /// zero tables and silently skip every migration. It must hard-fail
    /// Init instead, matching the adjacent `collections_to_tables` arm.
    #[tokio::test]
    async fn init_fails_when_collections_config_is_malformed() {
        let block = PostgresDatabaseBlock::new();
        let event = LifecycleEvent {
            event_type: LifecycleType::Init,
            data: serde_json::to_vec(&serde_json::json!({
                "collections": "not-an-array"
            }))
            .expect("serialize test config"),
        };

        let err = block
            .lifecycle(&NoopContext, event)
            .await
            .expect_err("malformed collections config must fail Init");

        assert_eq!(err.code, ErrorCode::FailedPrecondition);
        assert!(
            err.message.contains("collections"),
            "expected error message to mention collections, got: {}",
            err.message
        );
    }

    /// An Init event whose payload holds `url` under the block's URL key, if
    /// given — where the runtime puts the value its `ConfigSource` resolved.
    fn init_event(url: Option<&str>) -> LifecycleEvent {
        let payload = match url {
            Some(url) => serde_json::json!({ DATABASE_URL_ENV: url }),
            None => serde_json::json!({}),
        };
        LifecycleEvent {
            event_type: LifecycleType::Init,
            data: serde_json::to_vec(&payload).expect("serialize test config"),
        }
    }

    /// The URL comes from the Init payload, where the runtime puts the
    /// block's declared config var resolved through the embedder's
    /// `ConfigSource`. The context serves no config at all. The URL given
    /// here is refused by the connect step itself (it turns the statement
    /// cache off), so reaching that refusal proves Init read it.
    #[tokio::test]
    async fn init_reads_the_url_from_the_init_payload() {
        let err = PostgresDatabaseBlock::new()
            .lifecycle(
                &NoopContext,
                init_event(Some(
                    "postgres://u:p@127.0.0.1:1/db?statement-cache-capacity=0",
                )),
            )
            .await
            .expect_err("the configured URL is refused at connect");
        assert_eq!(err.code, ErrorCode::InvalidArgument, "{}", err.message);
        assert!(err.message.contains("statement cache"), "{}", err.message);
    }

    /// Without the config var (or with it empty) Init fails naming it.
    #[tokio::test]
    async fn init_without_a_configured_url_fails_naming_the_var() {
        for url in [None, Some("")] {
            let err = PostgresDatabaseBlock::new()
                .lifecycle(&NoopContext, init_event(url))
                .await
                .expect_err("no URL, no database");
            assert_eq!(err.code, ErrorCode::FailedPrecondition);
            assert!(err.message.contains(DATABASE_URL_ENV), "{}", err.message);
        }
    }

    /// The URL holds the database password (`postgres://user:pass@host/db`),
    /// so the declared var is sensitive: served config masks it.
    #[test]
    fn the_database_url_is_declared_sensitive() {
        use wafer_block::Block as _;

        let info = PostgresDatabaseBlock::new().info();
        let var = info
            .config_keys
            .iter()
            .find(|v| v.key == DATABASE_URL_ENV)
            .expect("the URL var is declared");
        assert!(var.is_sensitive(), "{var:?}");
    }
}
