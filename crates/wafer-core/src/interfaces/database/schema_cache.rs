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
//! ## Probe/invalidate linearizability (the TOCTOU guard)
//!
//! Populating the cache is inherently a two-step, `.await`-split dance: the
//! backend probes the database (which yields the task), then writes the result
//! back after resuming. A concurrent [`invalidate`](SchemaCache::invalidate) /
//! [`clear`](SchemaCache::clear) landing *inside* that gap must not be undone
//! by the write-back — otherwise a pre-mutation value (e.g. a negative
//! `exists=false` read just before a `CREATE TABLE`) is resurrected
//! permanently, since nothing re-probes a populated entry except the next
//! mutation on that exact table.
//!
//! [`SchemaCache`] closes this with a monotonic **generation** counter bumped
//! under the write lock on every `invalidate`/`clear`. A populating caller
//! snapshots [`generation`](SchemaCache::generation) *before* it probes and
//! writes back through
//! [`set_table_exists_if_gen`](SchemaCache::set_table_exists_if_gen) /
//! [`set_columns_if_gen`](SchemaCache::set_columns_if_gen), which — atomically
//! under the write lock — commit only if the generation is unchanged. A
//! write-back that raced a mutation is dropped (leaving the entry absent, so
//! the next op re-probes), never clobbering the mutation. Probe-population and
//! invalidation are thereby linearizable with respect to each other.
//!
//! # Concurrency
//!
//! Every method takes `&self` and holds the lock only for the duration of a
//! synchronous map operation — never across an `.await` — so it is sound for
//! the async SQL backends. Reads clone the small column vector out under the
//! lock rather than returning a borrow, keeping the critical section to a
//! single map lookup.

use std::collections::HashMap;

use parking_lot::RwLock;

/// Memoized introspection facts for one table. Each fact is independently
/// populated (`dbx_table_exists` fills `exists`, the column-list introspection
/// fills `columns`), so both are `Option` and `None` means "not yet probed".
#[derive(Debug, Default)]
struct TableSchema {
    /// Whether the table exists, once probed.
    exists: Option<bool>,
    /// Lowercased column names, once listed.
    columns: Option<Vec<String>>,
}

/// Lock-protected cache state: the per-table facts plus the generation counter
/// that guards racing write-backs (see the module docs). Keeping the counter
/// under the same lock as the map makes an invalidation's bump atomic with the
/// entry removal, and a gen-guarded write-back's check atomic with its insert.
#[derive(Debug, Default)]
struct Inner {
    generation: u64,
    tables: HashMap<String, TableSchema>,
}

/// Interior-mutable, thread-safe cache of per-table schema facts.
///
/// Backends store one of these and expose it through
/// [`DbExec::schema_cache`](super::exec::DbExec::schema_cache); the shared
/// executor consults it before every introspection and repopulates it on a
/// miss via the generation-guarded setters. See the module docs for the
/// invalidation and linearizability contract.
#[derive(Debug, Default)]
pub struct SchemaCache {
    inner: RwLock<Inner>,
}

impl SchemaCache {
    /// Create an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the current generation. A caller captures this *before* it
    /// probes the database, then passes it to the `*_if_gen` write-backs so a
    /// concurrent [`invalidate`](Self::invalidate)/[`clear`](Self::clear)
    /// (which bumps the generation) causes the stale write-back to be dropped.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.inner.read().generation
    }

    /// Cached table-exists fact, or `None` on a miss.
    #[must_use]
    pub fn table_exists(&self, table: &str) -> Option<bool> {
        self.inner.read().tables.get(table).and_then(|t| t.exists)
    }

    /// Record whether `table` exists, but only if the cache has not been
    /// mutated since `expected_gen` was snapshotted (see the module docs). A
    /// generation mismatch means an invalidation raced the probe, so the
    /// write-back is discarded.
    pub fn set_table_exists_if_gen(&self, table: &str, exists: bool, expected_gen: u64) {
        let mut inner = self.inner.write();
        if inner.generation != expected_gen {
            return;
        }
        inner.tables.entry(table.to_string()).or_default().exists = Some(exists);
    }

    /// Cached column list (lowercased), or `None` on a miss.
    #[must_use]
    pub fn columns(&self, table: &str) -> Option<Vec<String>> {
        self.inner
            .read()
            .tables
            .get(table)
            .and_then(|t| t.columns.clone())
    }

    /// Record the full lowercased column list for `table`, but only if the
    /// cache has not been mutated since `expected_gen` (see the module docs).
    ///
    /// A non-empty list also proves the table exists, so the exists fact is set
    /// alongside it; an empty list (a missing table's introspection result)
    /// leaves the exists fact untouched — the authoritative existence probe
    /// owns it.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the write guard covers the whole critical section — the \
                  generation check, the conditional exists-set and the \
                  columns-set mutate the same entry and are the entire body; \
                  there is nothing to tighten"
    )]
    pub fn set_columns_if_gen(&self, table: &str, columns: Vec<String>, expected_gen: u64) {
        let mut inner = self.inner.write();
        if inner.generation != expected_gen {
            return;
        }
        let entry = inner.tables.entry(table.to_string()).or_default();
        if !columns.is_empty() {
            entry.exists = Some(true);
        }
        entry.columns = Some(columns);
    }

    /// Invalidate every cached fact for `table` and bump the generation.
    /// Called after a targeted schema mutation (migration, drop, add-column,
    /// lazy `ALTER TABLE`): the next read re-introspects, and any write-back
    /// still in flight from before this call is dropped.
    pub fn invalidate(&self, table: &str) {
        let mut inner = self.inner.write();
        inner.tables.remove(table);
        inner.generation = inner.generation.wrapping_add(1);
    }

    /// Drop every cached entry and bump the generation. Called after raw DDL
    /// whose target table can't be determined from the SQL text (the
    /// `exec_raw`/DDL escape hatch), so no stale entry — or in-flight
    /// write-back — outlives a schema change.
    pub fn clear(&self) {
        let mut inner = self.inner.write();
        inner.tables.clear();
        inner.generation = inner.generation.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::SchemaCache;

    #[test]
    fn exists_miss_then_hit() {
        let cache = SchemaCache::new();
        let gen0 = cache.generation();
        assert_eq!(cache.table_exists("users"), None, "cold miss");
        cache.set_table_exists_if_gen("users", true, gen0);
        assert_eq!(cache.table_exists("users"), Some(true));
        cache.set_table_exists_if_gen("users", false, cache.generation());
        assert_eq!(cache.table_exists("users"), Some(false));
    }

    #[test]
    fn columns_miss_then_hit() {
        let cache = SchemaCache::new();
        let gen0 = cache.generation();
        assert_eq!(cache.columns("users"), None);
        cache.set_columns_if_gen("users", vec!["id".into(), "name".into()], gen0);
        assert_eq!(
            cache.columns("users"),
            Some(vec!["id".into(), "name".into()])
        );
    }

    #[test]
    fn non_empty_columns_imply_existence() {
        let cache = SchemaCache::new();
        cache.set_columns_if_gen("users", vec!["id".into()], cache.generation());
        assert_eq!(
            cache.table_exists("users"),
            Some(true),
            "a listed column set proves the table exists"
        );
    }

    #[test]
    fn empty_columns_do_not_touch_existence() {
        let cache = SchemaCache::new();
        cache.set_table_exists_if_gen("ghost", false, cache.generation());
        cache.set_columns_if_gen("ghost", Vec::new(), cache.generation());
        assert_eq!(
            cache.table_exists("ghost"),
            Some(false),
            "an empty column list must not overwrite the existence probe"
        );
    }

    #[test]
    fn invalidate_drops_only_the_named_table() {
        let cache = SchemaCache::new();
        cache.set_columns_if_gen("a", vec!["id".into()], cache.generation());
        cache.set_columns_if_gen("b", vec!["id".into()], cache.generation());
        cache.invalidate("a");
        assert_eq!(cache.columns("a"), None, "invalidated");
        assert_eq!(cache.table_exists("a"), None, "invalidated");
        assert_eq!(cache.columns("b"), Some(vec!["id".into()]), "untouched");
    }

    #[test]
    fn clear_drops_everything() {
        let cache = SchemaCache::new();
        cache.set_columns_if_gen("a", vec!["id".into()], cache.generation());
        cache.set_table_exists_if_gen("b", true, cache.generation());
        cache.clear();
        assert_eq!(cache.columns("a"), None);
        assert_eq!(cache.table_exists("b"), None);
    }

    #[test]
    fn invalidate_and_clear_bump_generation() {
        let cache = SchemaCache::new();
        let g0 = cache.generation();
        cache.invalidate("x");
        let g1 = cache.generation();
        assert_ne!(g1, g0, "invalidate must advance the generation");
        cache.clear();
        assert_ne!(cache.generation(), g1, "clear must advance the generation");
    }

    /// The core TOCTOU invariant: a write-back stamped with a generation that
    /// a concurrent invalidation has since advanced past is dropped, never
    /// resurrecting the pre-mutation value.
    #[test]
    fn stale_gen_write_back_is_discarded() {
        // Negative-cache-survives-mutation (Repro A) shape.
        let cache = SchemaCache::new();
        let gen0 = cache.generation(); // snapshotted "before the probe"

        // A concurrent invalidate lands during the probe's await gap.
        cache.invalidate("orders");

        // The probe resumes and tries to write back its now-stale read.
        cache.set_table_exists_if_gen("orders", false, gen0);
        assert_eq!(
            cache.table_exists("orders"),
            None,
            "a write-back racing an invalidate must be discarded, not cached"
        );

        // The column write-back is guarded identically.
        cache.set_columns_if_gen("orders", vec!["id".into()], gen0);
        assert_eq!(
            cache.columns("orders"),
            None,
            "a stale column write-back must be discarded too"
        );

        // A fresh probe (current generation) commits normally.
        cache.set_table_exists_if_gen("orders", true, cache.generation());
        assert_eq!(cache.table_exists("orders"), Some(true));
    }
}
