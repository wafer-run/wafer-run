use std::sync::Arc;

use wafer_run::block::{Block, BlockInfo};
use wafer_run::context::Context;
use wafer_run::schema::Table;
use wafer_run::types::*;
use wafer_run::Wafer;

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
        BlockInfo {
            name: "wafer-run/database".to_string(),
            version: "0.1.0".to_string(),
            interface: "database@v1".to_string(),
            summary: "Database service (SQL queries, CRUD, schema migrations)".to_string(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: BlockRuntime::Both,
            requires: Vec::new(),
            collections: Vec::new(),
        }
    }

    async fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
        handler::handle_message(self.service.as_ref(), msg).await
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
pub fn register_with(w: &mut Wafer, service: Arc<dyn DatabaseService>) {
    w.register_block("wafer-run/database", Arc::new(DatabaseBlock::new(service)));
}

/// Register with pre-built schema tables for migration.
pub fn register_with_tables(
    w: &mut Wafer,
    service: Arc<dyn DatabaseService>,
    tables: Vec<Table>,
) {
    w.register_block(
        "wafer-run/database",
        Arc::new(DatabaseBlock::with_tables(service, tables)),
    );
}
