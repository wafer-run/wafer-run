//! Per-backend schema-introspection cache.
//!
//! SQL [`DatabaseService`](super::service::DatabaseService) backends probe the
//! schema before most logical CRUD operations: a table-exists check plus a
//! column-list introspection query, issued *before* the actual data query. On
//! a local SQLite file those are cheap, but on Cloudflare D1 each is a network
//! round-trip that dwarfs the data query itself. [`SchemaCache`] memoizes both
//! facts per table — the column list together with which columns are declared
//! to hold JSON, which every row read decodes by — plus the table's primary
//! key, which a sorted or paged `list` appends to its `ORDER BY`, and whether
//! the table fills `id` itself, which `create` asks before minting one, so a
//! warm backend issues zero introspection round-trips in steady state.
//!
//! Only facts about a table that exists are kept. That a table is *missing*
//! is never memoized: another process (a second replica, a migration run out
//! of band) can create it at any moment, and nothing in this process would
//! learn of it — a cached "missing" would answer every read of that table as
//! empty for the life of the cache. A missing table therefore costs one
//! existence probe per operation against it.
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
//! `exec_raw` paths, and the column check's uncached re-read of a column list
//! that lacks a name a request uses) and in each backend's schema-management
//! methods (`ensure_schema_table`, `schema_drop_table`, `schema_add_column`).
//!
//! ## Probe/invalidate linearizability (the TOCTOU guard)
//!
//! Populating the cache is inherently a two-step, `.await`-split dance: the
//! backend probes the database (which yields the task), then writes the result
//! back after resuming. A concurrent [`invalidate`](SchemaCache::invalidate) /
//! [`clear`](SchemaCache::clear) landing *inside* that gap must not be undone
//! by the write-back — otherwise a pre-mutation value (e.g. a column list read
//! just before an `ALTER TABLE … ADD COLUMN`, or a table's presence read just
//! before a `DROP TABLE`) is resurrected permanently, since nothing re-probes
//! a populated entry except the next mutation on that exact table.
//!
//! [`SchemaCache`] closes this with a monotonic **generation** counter bumped
//! under the write lock on every `invalidate`/`clear`. A populating caller
//! snapshots [`generation`](SchemaCache::generation) *before* it probes and
//! writes back through
//! [`mark_table_present_if_gen`](SchemaCache::mark_table_present_if_gen) /
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

use super::codec::JsonColumns;

/// A table's columns as introspected: every column name (lowercased), and
/// the columns whose declared type holds JSON (see
/// [`codec`](super::codec)).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableColumns {
    /// Lowercased column names, in declaration order.
    pub names: Vec<String>,
    /// The columns declared to hold JSON.
    pub json: JsonColumns,
}

/// Memoized introspection facts for one table. Each fact is independently
/// populated (`dbx_table_exists` sets `present`, the column-list
/// introspection fills `columns`, the primary-key introspection fills
/// `primary_key`, the generated-id introspection fills `generates_id`), so
/// each optional fact's `None` means "not yet probed".
#[derive(Debug, Default)]
struct TableSchema {
    /// The table is known to exist. `false` means only "not known": absence
    /// is never recorded (see the module docs).
    present: bool,
    /// Column names and JSON columns, once listed.
    columns: Option<TableColumns>,
    /// Primary-key column names in key order, as the catalog spells them;
    /// empty for a table with no primary key.
    primary_key: Option<Vec<String>>,
    /// Whether the table fills `id` itself when an insert omits it.
    generates_id: Option<bool>,
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

    /// Whether `table` is known to exist. `false` is a miss, never an answer:
    /// the cache does not record that a table is missing (see the module
    /// docs), so the caller probes.
    #[must_use]
    pub fn table_known_present(&self, table: &str) -> bool {
        self.inner
            .read()
            .tables
            .get(table)
            .is_some_and(|t| t.present)
    }

    /// Record that `table` exists, but only if the cache has not been mutated
    /// since `expected_gen` was snapshotted (see the module docs). A
    /// generation mismatch means an invalidation raced the probe, so the
    /// write-back is discarded. There is no way to record that a table is
    /// missing.
    pub fn mark_table_present_if_gen(&self, table: &str, expected_gen: u64) {
        let mut inner = self.inner.write();
        if inner.generation != expected_gen {
            return;
        }
        inner.tables.entry(table.to_string()).or_default().present = true;
    }

    /// Cached columns, or `None` on a miss.
    #[must_use]
    pub fn columns(&self, table: &str) -> Option<TableColumns> {
        self.inner
            .read()
            .tables
            .get(table)
            .and_then(|t| t.columns.clone())
    }

    /// Record the full column list for `table`, but only if the cache has
    /// not been mutated since `expected_gen` (see the module docs).
    ///
    /// A non-empty list also proves the table exists, so the table is marked
    /// present alongside it. An empty list is a missing table's introspection result
    /// and is not recorded: every row read decodes by the cached list's JSON
    /// columns, and pinning "none" for a table a later migration creates
    /// would read its JSON columns back as text for the life of the cache.
    /// Presence stays with the authoritative existence probe.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the write guard covers the whole critical section — the \
                  generation check, the presence-set and the columns-set \
                  mutate the same entry and are the entire body; there is \
                  nothing to tighten"
    )]
    pub fn set_columns_if_gen(&self, table: &str, columns: TableColumns, expected_gen: u64) {
        if columns.names.is_empty() {
            return;
        }
        let mut inner = self.inner.write();
        if inner.generation != expected_gen {
            return;
        }
        let entry = inner.tables.entry(table.to_string()).or_default();
        entry.present = true;
        entry.columns = Some(columns);
    }

    /// Cached primary-key columns, or `None` on a miss.
    #[must_use]
    pub fn primary_key(&self, table: &str) -> Option<Vec<String>> {
        self.inner
            .read()
            .tables
            .get(table)
            .and_then(|t| t.primary_key.clone())
    }

    /// Record `table`'s primary-key columns, but only if the cache has not
    /// been mutated since `expected_gen` (see the module docs).
    ///
    /// A non-empty key proves the table exists, so the table is marked present
    /// alongside it. An empty key is ambiguous: the key introspection of a
    /// table with no primary key and of a table that does not exist yet both
    /// come back empty. It is recorded only when the entry already knows the
    /// table exists; otherwise it is dropped, so a table that a later
    /// migration creates with a key is re-introspected rather than listed
    /// without a tiebreak for the life of the cache.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the write guard covers the whole critical section — the \
                  generation check, the presence check and the key-set \
                  mutate the same entry and are the entire body"
    )]
    pub fn set_primary_key_if_gen(&self, table: &str, key: Vec<String>, expected_gen: u64) {
        let mut inner = self.inner.write();
        if inner.generation != expected_gen {
            return;
        }
        let entry = inner.tables.entry(table.to_string()).or_default();
        if !key.is_empty() {
            entry.present = true;
        } else if !entry.present {
            return;
        }
        entry.primary_key = Some(key);
    }

    /// Cached answer to "does `table` fill `id` itself?", or `None` on a miss.
    #[must_use]
    pub fn generates_id(&self, table: &str) -> Option<bool> {
        self.inner
            .read()
            .tables
            .get(table)
            .and_then(|t| t.generates_id)
    }

    /// Record whether `table` fills `id` itself, but only if the cache has not
    /// been mutated since `expected_gen` (see the module docs).
    ///
    /// `true` proves the table exists, so the table is marked present
    /// alongside it. `false` is also the answer for a table that does not
    /// exist yet, so it is recorded only when the entry already knows the
    /// table exists; otherwise it is dropped and the next lookup asks again.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the write guard covers the whole critical section — the \
                  generation check, the presence check and the fact-set \
                  mutate the same entry and are the entire body"
    )]
    pub fn set_generates_id_if_gen(&self, table: &str, generates_id: bool, expected_gen: u64) {
        let mut inner = self.inner.write();
        if inner.generation != expected_gen {
            return;
        }
        let entry = inner.tables.entry(table.to_string()).or_default();
        if generates_id {
            entry.present = true;
        } else if !entry.present {
            return;
        }
        entry.generates_id = Some(generates_id);
    }

    /// Invalidate every cached fact for `table` and bump the generation.
    /// Called after a targeted schema mutation (migration, drop, add-column,
    /// lazy `ALTER TABLE`), or before re-reading a column list that may be
    /// stale: the next read re-introspects, and any write-back still in
    /// flight from before this call is dropped.
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
    use super::{SchemaCache, TableColumns};

    fn cols(names: &[&str]) -> TableColumns {
        TableColumns {
            names: names.iter().map(ToString::to_string).collect(),
            json: Default::default(),
        }
    }

    #[test]
    fn presence_miss_then_hit() {
        let cache = SchemaCache::new();
        let gen0 = cache.generation();
        assert!(!cache.table_known_present("users"), "cold miss");
        cache.mark_table_present_if_gen("users", gen0);
        assert!(cache.table_known_present("users"));
    }

    #[test]
    fn columns_miss_then_hit() {
        let cache = SchemaCache::new();
        let gen0 = cache.generation();
        assert_eq!(cache.columns("users"), None);
        cache.set_columns_if_gen("users", cols(&["id", "name"]), gen0);
        assert_eq!(cache.columns("users"), Some(cols(&["id", "name"])));
    }

    #[test]
    fn non_empty_columns_imply_existence() {
        let cache = SchemaCache::new();
        cache.set_columns_if_gen("users", cols(&["id"]), cache.generation());
        assert!(
            cache.table_known_present("users"),
            "a listed column set proves the table exists"
        );
    }

    #[test]
    fn empty_columns_are_not_cached_and_do_not_mark_presence() {
        let cache = SchemaCache::new();
        cache.set_columns_if_gen("ghost", cols(&[]), cache.generation());
        assert!(
            !cache.table_known_present("ghost"),
            "an empty column list proves nothing about the table"
        );
        assert_eq!(
            cache.columns("ghost"),
            None,
            "a missing table's empty column list is not cached, so the table's \
             columns are read once a migration creates it"
        );
    }

    #[test]
    fn primary_key_miss_then_hit_and_dropped_by_invalidate() {
        let c = SchemaCache::new();
        assert_eq!(c.primary_key("t"), None);
        c.set_primary_key_if_gen("t", vec!["id".into()], c.generation());
        assert_eq!(c.primary_key("t"), Some(vec!["id".to_string()]));
        assert!(c.table_known_present("t"), "a key proves the table");
        // An empty key for a table not known to exist may be a table that
        // does not exist yet: it is dropped, so the next lookup re-probes.
        c.set_primary_key_if_gen("later", Vec::new(), c.generation());
        assert_eq!(
            c.primary_key("later"),
            None,
            "a missing table's key is not cached"
        );
        // Once the table is known to exist, an empty key is a cached answer
        // ("no primary key"), not a miss.
        c.mark_table_present_if_gen("keyless", c.generation());
        c.set_primary_key_if_gen("keyless", Vec::new(), c.generation());
        assert_eq!(c.primary_key("keyless"), Some(Vec::new()));
        let stale = c.generation();
        c.invalidate("t");
        assert_eq!(c.primary_key("t"), None);
        c.set_primary_key_if_gen("t", vec!["old".into()], stale);
        assert_eq!(c.primary_key("t"), None, "a raced write-back is dropped");
    }

    #[test]
    fn generates_id_is_cached_only_for_a_table_known_to_exist() {
        let c = SchemaCache::new();
        assert_eq!(c.generates_id("t"), None);
        c.set_generates_id_if_gen("t", true, c.generation());
        assert_eq!(c.generates_id("t"), Some(true));
        assert!(
            c.table_known_present("t"),
            "a generated id proves the table"
        );
        // "No" is also a missing table's answer: dropped until the table is
        // known to exist.
        c.set_generates_id_if_gen("later", false, c.generation());
        assert_eq!(c.generates_id("later"), None);
        c.mark_table_present_if_gen("later", c.generation());
        c.set_generates_id_if_gen("later", false, c.generation());
        assert_eq!(c.generates_id("later"), Some(false));
        let stale = c.generation();
        c.invalidate("t");
        assert_eq!(c.generates_id("t"), None);
        c.set_generates_id_if_gen("t", true, stale);
        assert_eq!(c.generates_id("t"), None, "a raced write-back is dropped");
    }

    #[test]
    fn invalidate_drops_only_the_named_table() {
        let cache = SchemaCache::new();
        cache.set_columns_if_gen("a", cols(&["id"]), cache.generation());
        cache.set_columns_if_gen("b", cols(&["id"]), cache.generation());
        cache.invalidate("a");
        assert_eq!(cache.columns("a"), None, "invalidated");
        assert!(!cache.table_known_present("a"), "invalidated");
        assert_eq!(cache.columns("b"), Some(cols(&["id"])), "untouched");
    }

    #[test]
    fn clear_drops_everything() {
        let cache = SchemaCache::new();
        cache.set_columns_if_gen("a", cols(&["id"]), cache.generation());
        cache.mark_table_present_if_gen("b", cache.generation());
        cache.clear();
        assert_eq!(cache.columns("a"), None);
        assert!(!cache.table_known_present("b"));
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
        let cache = SchemaCache::new();
        let gen0 = cache.generation(); // snapshotted "before the probe"

        // A concurrent invalidate (a DROP TABLE) lands during the probe's
        // await gap.
        cache.invalidate("orders");

        // The probe resumes and tries to write back its now-stale read.
        cache.mark_table_present_if_gen("orders", gen0);
        assert!(
            !cache.table_known_present("orders"),
            "a write-back racing an invalidate must be discarded, not cached"
        );

        // The column write-back is guarded identically.
        cache.set_columns_if_gen("orders", cols(&["id"]), gen0);
        assert_eq!(
            cache.columns("orders"),
            None,
            "a stale column write-back must be discarded too"
        );

        // A fresh probe (current generation) commits normally.
        cache.mark_table_present_if_gen("orders", cache.generation());
        assert!(cache.table_known_present("orders"));
    }
}
