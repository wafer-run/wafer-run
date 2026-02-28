use crate::manifest::{self, BlockManifest};
use crate::schema::adapter::SchemaAdapter;

/// Run applies database schema from all block manifests using the given adapter.
pub fn run(manifests: &[BlockManifest], adapter: &dyn SchemaAdapter) -> Result<(), String> {
    for m in manifests {
        let tables = manifest::to_schema_tables(m);
        if tables.is_empty() {
            continue;
        }
        adapter
            .ensure_tables(&tables)
            .map_err(|e| format!("migrate block {}: {}", m.name, e))?;
    }
    Ok(())
}
