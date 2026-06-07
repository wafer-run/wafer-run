//! PostgreSQL database block — `wafer-run/postgres`.
//!
//! Self-contained block wrapping the PostgreSQL database service.
//! Uses the shared database message handler for the `database@v1` interface.

#![warn(missing_docs)]

/// PostgreSQL implementation of `wafer_core::interfaces::database::service::DatabaseService`.
///
/// Exposed publicly so native consumers (e.g. `solobase-native`) can construct
/// the service directly from a connection URL when running outside the
/// block lifecycle.
pub mod service;

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use service::PostgresDatabaseService;
use wafer_block::{
    Block, BlockCategory, BlockConfig, BlockInfo, ConfigVar, Context, InputStream, LifecycleEvent,
    LifecycleType, Message, OutputStream, WaferError,
};
use wafer_block_macro::wafer_async_trait;
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_schema::{
    manifest::{collections_to_tables, CollectionDef},
    Table,
};

const DATABASE_URL_ENV: &str = "WAFER_RUN__POSTGRES__DATABASE_URL";

/// The PostgreSQL database block.
///
/// Initialized during `lifecycle(Init)`. Reads its connection URL from the
/// `WAFER_RUN__POSTGRES__DATABASE_URL` env var — a wafer-run process
/// typically points at one database, so this lives in `config_keys`.
pub(crate) struct PostgresDatabaseBlock {
    service: OnceLock<Arc<dyn DatabaseService>>,
    tables: OnceLock<Vec<Table>>,
}

impl Default for PostgresDatabaseBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl PostgresDatabaseBlock {
    /// Construct a fresh block with empty (uninitialized) service and table state.
    pub(crate) fn new() -> Self {
        Self {
            service: OnceLock::new(),
            tables: OnceLock::new(),
        }
    }
}

#[wafer_async_trait]
impl Block for PostgresDatabaseBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/postgres",
            "0.0.1",
            "database@v1",
            "PostgreSQL database block",
        )
        .category(BlockCategory::Infrastructure)
        .config_keys(vec![ConfigVar::new(
            DATABASE_URL_ENV,
            "PostgreSQL connection URL (postgres://user:pass@host:port/db). \
             Required.",
            "",
        )
        .name("Database URL")])
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let service = self
            .service
            .get()
            .expect("wafer-run/postgres: not initialized — call lifecycle(Init) first");
        let body = input.collect_to_bytes().await;
        wafer_core::interfaces::database::handler::handle_message(service.as_ref(), &msg, &body)
            .await
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init && self.service.get().is_none() {
            let config = BlockConfig::from_event(&event);

            let tables = match config.get("collections") {
                Some(v) => {
                    match serde_json::from_value::<HashMap<String, CollectionDef>>(v.clone()) {
                        Ok(colls) => collections_to_tables(&colls).map_err(|e| {
                            WaferError::new(
                                "config",
                                format!("wafer-run/postgres: invalid collections config: {e}"),
                            )
                        })?,
                        Err(e) => {
                            tracing::error!(
                                error = %e,
                                "failed to parse database collections config"
                            );
                            Vec::new()
                        }
                    }
                }
                None => Vec::new(),
            };
            self.tables.set(tables).ok();

            let url = std::env::var(DATABASE_URL_ENV).map_err(|_| {
                WaferError::new(
                    "config",
                    format!("wafer-run/postgres: {DATABASE_URL_ENV} must be set"),
                )
            })?;

            let svc = PostgresDatabaseService::connect(&url)
                .await
                .map_err(|e| WaferError::new("init", format!("wafer-run/postgres: {e}")))?;
            tracing::info!("PostgreSQL database connected");
            self.service.set(Arc::new(svc)).ok();
        }

        // Run table migrations on Init
        if event.event_type == LifecycleType::Init {
            let tables = self.tables.get().map_or(&[][..], |t| t.as_slice());
            if let Some(service) = self.service.get() {
                wafer_core::interfaces::database::handler::handle_lifecycle(
                    service.as_ref(),
                    tables,
                    &event,
                )
                .await?;
            }
        }

        Ok(())
    }
}

wafer_block::register_static_block!("wafer-run/postgres", PostgresDatabaseBlock);
