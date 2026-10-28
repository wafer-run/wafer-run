use std::sync::Arc;

use wafer_block::{BlockRegistry, RuntimeError};
use wafer_schema::Table;

use crate::interfaces::database::{handler, service::DatabaseService};

crate::service_block! {
    /// Unified database block. Wraps any `DatabaseService` implementation.
    block: pub DatabaseBlock,
    name: "wafer-run/database",
    version: "0.0.1",
    interface: "database@v1",
    description: "Database service (SQL queries, CRUD, schema migrations)",
    category: Service,
    fields: { service: Arc<dyn DatabaseService> },
    extra_fields: { tables: Vec<Table> },
    handle: |this, ctx, msg, body| {
        handler::handle_message(this.service.as_ref(), ctx, &msg, &body).await
    },
    lifecycle: |this, _ctx, event| {
        handler::handle_lifecycle(this.service.as_ref(), &this.tables, &event).await
    },
}

impl DatabaseBlock {
    /// Create with pre-built schema tables for migration during lifecycle Init.
    pub fn with_tables(service: Arc<dyn DatabaseService>, tables: Vec<Table>) -> Self {
        Self { service, tables }
    }
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
