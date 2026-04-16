use std::sync::Arc;

use wafer_block::block::Block;
use wafer_block::context::Context;
use wafer_block::streams::input::InputStream;
use wafer_block::streams::output::OutputStream;
use wafer_block::types::BlockInfo;
use wafer_block::BlockRegistry;
use wafer_block::RuntimeError;
use wafer_block::*;
use wafer_run::schema::Table;

use crate::interfaces::database::{handler, service::DatabaseService};

/// Unified database block. Wraps any `DatabaseService` implementation.
pub struct DatabaseBlock {
    service: Arc<dyn DatabaseService>,
    tables: Vec<Table>,
}

impl DatabaseBlock {
    pub fn new(service: Arc<dyn DatabaseService>) -> Self {
        Self {
            service,
            tables: Vec::new(),
        }
    }

    /// Create with pre-built schema tables for migration during lifecycle Init.
    pub fn with_tables(service: Arc<dyn DatabaseService>, tables: Vec<Table>) -> Self {
        Self { service, tables }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for DatabaseBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/database",
            "0.0.1",
            "database@v1",
            "Database service (SQL queries, CRUD, schema migrations)",
        )
        .category(BlockCategory::Service)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let body = input.collect_to_bytes().await;
        handler::handle_message(self.service.as_ref(), &msg, &body).await
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        handler::handle_lifecycle(self.service.as_ref(), &self.tables, &event).await
    }
}

/// Register the unified database block with the given service.
pub fn register_with(
    w: &mut dyn BlockRegistry,
    service: Arc<dyn DatabaseService>,
) -> Result<(), RuntimeError> {
    w.register_block("wafer-run/database", Arc::new(DatabaseBlock::new(service)))
}

/// Register with pre-built schema tables for migration.
pub fn register_with_tables(
    w: &mut dyn BlockRegistry,
    service: Arc<dyn DatabaseService>,
    tables: Vec<Table>,
) -> Result<(), RuntimeError> {
    w.register_block(
        "wafer-run/database",
        Arc::new(DatabaseBlock::with_tables(service, tables)),
    )
}
