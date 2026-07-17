use std::sync::Arc;

use wafer_block::{BlockRegistry, ConfigVar, InputType, RuntimeError};
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
    info_extras: |_this, info| info.config_keys(vec![ConfigVar::new(
        handler::STRICT_SCHEMA_CONFIG_KEY,
        "When \"true\", the database service trusts its migrated schema: it \
         skips the per-operation table-exists probe and the lazy ADD COLUMN \
         path, removing schema-introspection round-trips from the hot path \
         (a large win on Cloudflare D1, where each is a network round-trip). \
         Leave \"false\" (the default) to keep the self-healing lazy-schema \
         behavior for development. Applied once at Init.",
        "false",
    )
    .name("Strict Schema")
    .input_type(InputType::Toggle)]),
    handle: |this, ctx, msg, body| {
        handler::handle_message(this.service.as_ref(), ctx, &msg, &body).await
    },
    lifecycle: |this, ctx, event| {
        handler::handle_lifecycle(this.service.as_ref(), &this.tables, ctx, &event).await
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
