//! Per-backend schema-introspection cache.
//!
//! SQL [`DatabaseService`](super::service::DatabaseService) backends probe the
//! schema before most logical CRUD operations: a table-exists check plus a
//! column-list introspection query, issued *before* the actual data query. On
//! a local SQLite file those are cheap, but on Cloudflare D1 each is a network
//! round-trip that dwarfs the data query itself. [`SchemaCache`] memoizes both
//! facts per table so a warm backend issues zero introspection round-trips in
//! steady state.
//!
//! # Correctness
//!
//! The cache mirrors durable schema state, so **every schema mutation must
//! invalidate the affected table's entry** — or, for raw DDL whose target
//! can't be recovered from the SQL text, [`clear`](SchemaCache::clear) the
//! whole cache. A stale entry after an `ALTER TABLE` would let the executor
//! build SQL against a column set that no longer matches the database: a
//! correctness bug, not merely a performance one. The invalidation call sites
//! live in [`DbExec`](super::exec::DbExec) (the shared lazy-column-add and
//! `exec_raw` paths) and in each backend's schema-management methods
//! (`ensure_schema_table`, `schema_drop_table`, `schema_add_column`).
//!
//! # Concurrency
//!
//! The cache lives behind the backend's shared `&self`. Every method takes
//! `&self` and holds the lock only for the duration of a synchronous map
//! operation — never across an `.await` — so it is sound for the async SQL
//! backends. Reads clone the small column vector out under the lock rather
//! than returning a borrow, keeping the critical section to a single map
//! lookup.

use std::collections::HashMap;

use parking_lot::RwLock;

/// Memoized introspection facts for one table. Each fact is independently
/// populated (`dbx_table_exists` fills `exists`, the column-list introspection
/// fills `columns`), so both are `Option` and `None` means "not yet probed".
#[derive(Debug, Default, Clone)]
struct TableSchema {
    /// Whether the table exists, once probed.
    exists: Option<bool>,
    /// Lowercased column names, once listed.
    columns: Option<Vec<String>>,
}

/// Interior-mutable, thread-safe cache of per-table schema facts.
///
/// Backends store one of these and expose it through
/// [`DbExec::schema_cache`](super::exec::DbExec::schema_cache); the shared
/// executor consults it before every introspection and repopulates it on a
/// miss. See the module docs for the invalidation contract.
#[derive(Debug, Default)]
pub struct SchemaCache {
    tables: RwLock<HashMap<String, TableSchema>>,
}

impl SchemaCache {
    /// Create an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Cached table-exists fact, or `None` on a miss.
    #[must_use]
    pub fn table_exists(&self, table: &str) -> Option<bool> {
        self.tables.read().get(table).and_then(|t| t.exists)
    }

    /// Record whether `table` exists.
    pub fn set_table_exists(&self, table: &str, exists: bool) {
        self.tables
            .write()
            .entry(table.to_string())
            .or_default()
            .exists = Some(exists);
    }

    /// Cached column list (lowercased), or `None` on a miss.
    #[must_use]
    pub fn columns(&self, table: &str) -> Option<Vec<String>> {
        self.tables
            .read()
            .get(table)
            .and_then(|t| t.columns.clone())
    }

    /// Record the full lowercased column list for `table`. A non-empty list
    /// also proves the table exists, so the exists fact is set alongside it;
    /// an empty list (a missing table's introspection result) leaves the
    /// exists fact untouched — the authoritative existence probe owns it.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the write guard covers the whole critical section — the \
                  conditional exists-set and the columns-set mutate the same \
                  entry and are the entire body; there is nothing to tighten"
    )]
    pub fn set_columns(&self, table: &str, columns: Vec<String>) {
        let mut guard = self.tables.write();
        let entry = guard.entry(table.to_string()).or_default();
        if !columns.is_empty() {
            entry.exists = Some(true);
        }
        entry.columns = Some(columns);
    }

    /// Invalidate every cached fact for `table`. Called after a targeted
    /// schema mutation (migration, drop, add-column, lazy `ALTER TABLE`): the
    /// next read re-introspects the true schema.
    pub fn invalidate(&self, table: &str) {
        self.tables.write().remove(table);
    }

    /// Drop every cached entry. Called after raw DDL whose target table can't
    /// be determined from the SQL text (the `exec_raw`/DDL escape hatch), so
    /// no stale entry outlives a schema change.
    pub fn clear(&self) {
        self.tables.write().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::SchemaCache;

    #[test]
    fn exists_miss_then_hit() {
        let cache = SchemaCache::new();
        assert_eq!(cache.table_exists("users"), None, "cold miss");
        cache.set_table_exists("users", true);
        assert_eq!(cache.table_exists("users"), Some(true));
        cache.set_table_exists("users", false);
        assert_eq!(cache.table_exists("users"), Some(false));
    }

    #[test]
    fn columns_miss_then_hit() {
        let cache = SchemaCache::new();
        assert_eq!(cache.columns("users"), None);
        cache.set_columns("users", vec!["id".into(), "name".into()]);
        assert_eq!(
            cache.columns("users"),
            Some(vec!["id".into(), "name".into()])
        );
    }

    #[test]
    fn non_empty_columns_imply_existence() {
        let cache = SchemaCache::new();
        cache.set_columns("users", vec!["id".into()]);
        assert_eq!(
            cache.table_exists("users"),
            Some(true),
            "a listed column set proves the table exists"
        );
    }

    #[test]
    fn empty_columns_do_not_touch_existence() {
        let cache = SchemaCache::new();
        cache.set_table_exists("ghost", false);
        cache.set_columns("ghost", Vec::new());
        assert_eq!(
            cache.table_exists("ghost"),
            Some(false),
            "an empty column list must not overwrite the existence probe"
        );
    }

    #[test]
    fn invalidate_drops_only_the_named_table() {
        let cache = SchemaCache::new();
        cache.set_columns("a", vec!["id".into()]);
        cache.set_columns("b", vec!["id".into()]);
        cache.invalidate("a");
        assert_eq!(cache.columns("a"), None, "invalidated");
        assert_eq!(cache.table_exists("a"), None, "invalidated");
        assert_eq!(cache.columns("b"), Some(vec!["id".into()]), "untouched");
    }

    #[test]
    fn clear_drops_everything() {
        let cache = SchemaCache::new();
        cache.set_columns("a", vec!["id".into()]);
        cache.set_table_exists("b", true);
        cache.clear();
        assert_eq!(cache.columns("a"), None);
        assert_eq!(cache.table_exists("b"), None);
    }
}
