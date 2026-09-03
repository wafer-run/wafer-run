//! Block capability declarations and enforcement policy.
//!
//! See the wafer-site page "Block capabilities" for the high-level
//! model. TL;DR:
//!
//! - Blocks declare required capabilities in `BlockInfo::capabilities`.
//! - Operators narrow via a `capabilities` subkey in block config.
//! - The runtime intersects declared ∩ config and enforces on WASM blocks.
//! - Native blocks' declarations are documentation-only.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// A capability allowlist with an explicit three-state shape (SEC-06).
///
/// Replaces the old `bool` gate + companion `Vec`/`HashSet` where an **empty**
/// container ambiguously meant "any". There is deliberately no "empty means
/// any" here:
///
/// - [`Allowlist::None`] — capability disabled; denies everything.
/// - [`Allowlist::Any`] — capability enabled with no value restriction; allows
///   everything.
/// - [`Allowlist::Only`] — capability enabled, restricted to exactly this set;
///   an empty `Only` denies everything (not "any").
///
/// The value-matching rule is per-capability (network uses URL-prefix matching,
/// config uses exact key match), so matching lives in the enforcement methods
/// on [`BlockCapabilities`]; this type only models the tri-state and its
/// narrowing intersection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Allowlist {
    /// Capability disabled — deny everything.
    None,
    /// Capability enabled, no value restriction — allow everything.
    Any,
    /// Capability enabled, restricted to exactly these values (empty = deny).
    Only(BTreeSet<String>),
}

impl Default for Allowlist {
    /// Fail-closed: an unspecified allowlist denies everything.
    fn default() -> Self {
        Allowlist::None
    }
}

impl Allowlist {
    /// Whether the capability is enabled at all (`Any` or `Only`, not `None`).
    pub fn is_enabled(&self) -> bool {
        !matches!(self, Allowlist::None)
    }

    /// Whether `value` is permitted by exact membership: `None` → false,
    /// `Any` → true, `Only(set)` → `set.contains(value)`. Used by every
    /// capability whose allowlist matches values exactly (collections,
    /// storage folders, vector indexes, callable blocks, config keys). Network
    /// does NOT use this — it matches URL prefixes, not exact strings.
    pub fn allows(&self, value: &str) -> bool {
        match self {
            Allowlist::None => false,
            Allowlist::Any => true,
            Allowlist::Only(set) => set.contains(value),
        }
    }

    /// Narrowing intersection (declared ∩ operator-override) for allowlists
    /// whose `Only` entries match values EXACTLY: the result permits a value
    /// only if BOTH sides do. Symmetric.
    ///
    /// - `None` on either side → `None` (disabled wins).
    /// - `Any` ∩ x → x (the more specific side).
    /// - `Only(a)` ∩ `Only(b)` → `Only(a ∩ b)` (may be empty = deny-all, which
    ///   is unambiguous here — no "empty means any" to invert).
    ///
    /// `storage_folders` matches by path PREFIX, not exactly, so it narrows
    /// through [`Allowlist::intersect_path_prefix`] instead; a plain set
    /// intersection would silently drop a nested entry that both sides do in
    /// fact permit.
    pub fn intersect(&self, other: &Allowlist) -> Allowlist {
        match (self, other) {
            (Allowlist::None, _) | (_, Allowlist::None) => Allowlist::None,
            (Allowlist::Any, x) | (x, Allowlist::Any) => x.clone(),
            (Allowlist::Only(a), Allowlist::Only(b)) => {
                Allowlist::Only(a.intersection(b).cloned().collect())
            }
        }
    }

    /// Narrowing intersection for a PATH-PREFIX allowlist — the shape
    /// `BlockCapabilities::storage_folders` uses, where an entry admits every
    /// resource beneath it (`site/jhg` admits `site/jhg/a.txt`).
    ///
    /// `None`/`Any` behave exactly as in [`Allowlist::intersect`]. For
    /// `Only(a)` ∩ `Only(b)`, a pair of entries survives as the NARROWER of
    /// the two whenever one lies beneath the other, so:
    ///
    /// - an override entry nested under a declared entry survives (the
    ///   operator narrowed `site` to `site/jhg`), and
    /// - a declared entry nested under an override entry survives too (the
    ///   operator's broader `site` cannot widen the block's `site/jhg`).
    ///
    /// Entries with no covering relationship drop out, so the result still
    /// permits only what BOTH sides permit — a plain set intersection would
    /// have collapsed both nesting cases to deny-all.
    pub fn intersect_path_prefix(&self, other: &Allowlist) -> Allowlist {
        match (self, other) {
            (Allowlist::None, _) | (_, Allowlist::None) => Allowlist::None,
            (Allowlist::Any, x) | (x, Allowlist::Any) => x.clone(),
            (Allowlist::Only(a), Allowlist::Only(b)) => {
                let mut out = BTreeSet::new();
                for x in a {
                    for y in b {
                        if path_prefix_covers(y, x) {
                            out.insert(x.clone());
                        } else if path_prefix_covers(x, y) {
                            out.insert(y.clone());
                        }
                    }
                }
                Allowlist::Only(out)
            }
        }
    }
}

/// Whether the allowlist entry `entry` covers the storage resource
/// `resource`: they are equal, or `resource` lies beneath `entry` as a
/// `/`-separated path (`site/jhg` covers `site/jhg/a.txt` and
/// `site/jhg/sub/b`, but never `site/jhgx/a.txt`).
///
/// Degenerate entries cover NOTHING: an empty entry, or one ending in `/`,
/// would otherwise prefix-match by accident rather than by intent. Together
/// with the traversal-safety rule on the resource side (see
/// [`BlockCapabilities::allows_storage_folder`]) this keeps prefix matching a
/// statement about whole path segments only.
fn path_prefix_covers(entry: &str, resource: &str) -> bool {
    if entry.is_empty() || entry.ends_with('/') {
        return false;
    }
    entry == resource
        || resource
            .strip_prefix(entry)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Policy for which headers a block may read, write, or which should be masked.
///
/// Applied by the runtime only to WASM blocks. For native blocks, this is
/// documentation / inspector metadata only — enforcement is WASM-specific.
///
/// Default-denied sensitive header set (see
/// `wafer_run::wasm::wasmi_loader::default_sensitive_headers`) is masked
/// unless explicitly listed in `readable` (for inbound) or `writable`
/// (for outbound). `masked` adds extra headers to the deny set in both
/// directions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HeaderPolicy {
    /// Sensitive inbound headers the block may READ.
    /// Example: `["authorization"]`.
    #[serde(default)]
    pub readable: Vec<String>,

    /// Sensitive outbound headers the block may WRITE.
    /// Example: `["set-cookie"]`.
    #[serde(default)]
    pub writable: Vec<String>,

    /// Additional headers to mask beyond the default sensitive set.
    /// Applies to both directions. Operator extension for app-specific
    /// sensitive headers.
    /// Example: `["x-internal-token"]`.
    #[serde(default)]
    pub masked: Vec<String>,
}

/// BlockCapabilities declares what platform services a WASM block may access.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BlockCapabilities {
    /// Allowed database collections: `None` = none, `Any` = all, `Only([...])`
    /// = exactly these (empty `Only` = none). Replaces the old `HashSet` where
    /// `"*"` meant all — see [`Allowlist`].
    #[serde(default)]
    pub collections: Allowlist,
    /// Can use query_raw/exec_raw.
    #[serde(default)]
    pub raw_sql: bool,
    /// Can issue RAW DDL via `db::ddl()` (an arbitrary `CREATE TABLE` /
    /// `CREATE INDEX` / `DROP` statement).
    /// Convention: blocks only DDL their own (`{org}__{block}__*`) tables; this
    /// is enforced by code review + the WRAP-grant audit script, not by parsing
    /// SQL. Default `false` to keep `none()` fully sandboxed; native blocks get
    /// `true` via `unrestricted()`.
    ///
    /// Does NOT gate the structured schema ops — see [`Self::schema`].
    #[serde(default)]
    pub ddl: bool,
    /// Structured schema operations — `database.ensure_table` /
    /// `database.add_column` / `database.drop_table` — on collections this
    /// block may access; does NOT grant raw `database.ddl`.
    ///
    /// The ops take a validated `TableDef`/`ColumnDef` and the host builds the
    /// statement, so a block that only needs its own tables created gets this
    /// instead of the arbitrary-statement channel [`Self::ddl`] opens. They are
    /// authorized twice: on the table name (so `collections` still scopes
    /// *which* tables) and on `wrap::SCHEMA_RESOURCE`, which this field maps
    /// to. Default `false`; native blocks get `true` via `unrestricted()`.
    #[serde(default)]
    pub schema: bool,
    /// Allowed storage folders — or individual object paths:
    /// `None`/`Any`/`Only([...])` (see [`Allowlist`]).
    ///
    /// The storage service authorizes each op on `"{folder}/{key}"` (folder
    /// ops on the bare folder name), and an `Only` entry admits a resource
    /// when it is equal to the entry or lies beneath it as a `/`-separated
    /// path — see [`BlockCapabilities::allows_storage_folder`]. So
    /// `"uploads"` grants every key in `uploads/`, while `"uploads/logo.png"`
    /// grants exactly that object.
    ///
    /// Two rules keep prefix matching from over-admitting:
    ///
    /// - A resource with any EMPTY, `.` or `..` segment is refused outright,
    ///   whatever the allowlist says. Nothing normalizes the path, so
    ///   `site/jhg/../other` textually sits under a `site/jhg` grant while
    ///   naming a sibling folder.
    /// - An `Only` entry that is empty or ends in `/` matches NOTHING. Both
    ///   shapes would prefix-match by accident rather than by intent; a
    ///   folder is named without its trailing separator (`"uploads"`, not
    ///   `"uploads/"`).
    #[serde(default)]
    pub storage_folders: Allowlist,
    /// Can use crypto service.
    #[serde(default)]
    pub crypto: bool,
    /// Network access (SEC-06): `None` = no network, `Any` = any URL,
    /// `Only([prefixes])` = only URLs matching an allow-prefix. Replaces the
    /// old `network: bool` + `network_allow: Vec` whose empty list ambiguously
    /// meant "any".
    #[serde(default)]
    pub network: Allowlist,
    /// Config access (SEC-06): `None` = no config, `Any` = any key,
    /// `Only([keys])` = only these keys. Replaces the old `config: bool` +
    /// `config_keys: HashSet` whose empty set ambiguously meant "any".
    #[serde(default)]
    pub config: Allowlist,
    /// Allowed vector indexes (by storage name): `None`/`Any`/`Only([...])`
    /// (see [`Allowlist`]).
    #[serde(default)]
    pub vector_indexes: Allowlist,
    /// Blocks that may be called via `call_block()`: `None` = no calls,
    /// `Any` = any block, `Only([...])` = only these (see [`Allowlist`]).
    #[serde(default)]
    pub callable_blocks: Allowlist,
    /// Per-header read/write/mask policy.
    #[serde(default)]
    pub headers: HeaderPolicy,
}

impl BlockCapabilities {
    /// Unrestricted capabilities — used by native Rust blocks. All services
    /// allowed, all wildcards set, no allowlists enforced.
    pub fn unrestricted() -> Self {
        Self {
            collections: Allowlist::Any,
            raw_sql: true,
            ddl: true,
            schema: true,
            storage_folders: Allowlist::Any,
            crypto: true,
            network: Allowlist::Any,
            config: Allowlist::Any,
            vector_indexes: Allowlist::Any,
            callable_blocks: Allowlist::Any,
            headers: HeaderPolicy::default(),
        }
    }

    /// No capabilities — completely sandboxed default for untrusted WASM blocks.
    pub fn none() -> Self {
        Self {
            collections: Allowlist::None,
            raw_sql: false,
            ddl: false,
            schema: false,
            storage_folders: Allowlist::None,
            crypto: false,
            network: Allowlist::None,
            config: Allowlist::None,
            vector_indexes: Allowlist::None,
            callable_blocks: Allowlist::None, // None = no calls allowed
            headers: HeaderPolicy::default(),
        }
    }

    /// Whether this capability set permits operations against `collection`
    /// (matches `"*"` wildcard or an exact entry).
    pub fn allows_collection(&self, collection: &str) -> bool {
        self.collections.allows(collection)
    }

    /// Whether this capability set permits operations on the storage
    /// `resource` — `"{folder}/{key}"` for object ops, the bare folder name
    /// for folder ops.
    ///
    /// - A `resource` with any EMPTY, `.` or `..` segment → denied, whatever
    ///   the allowlist holds (including [`Allowlist::Any`]). Prefix matching
    ///   is textual and nothing normalizes the path, so `site/jhg/../other`
    ///   would otherwise pass a `site/jhg` grant while naming a sibling
    ///   folder. See [`crate::wrap::is_traversal_safe_path`]; the storage
    ///   handler refuses the same shape one layer earlier with
    ///   `InvalidArgument`.
    /// - [`Allowlist::None`] → denied.
    /// - [`Allowlist::Any`] → allowed.
    /// - [`Allowlist::Only`] → allowed iff some entry equals `resource` or is
    ///   a prefix of it terminated by `/`. The separator is required, so
    ///   `"site/jhg"` admits `"site/jhg/a.txt"` and `"site/jhg/sub/b"` but
    ///   never `"site/jhgx/a.txt"`. An entry that is empty or ends in `/`
    ///   matches nothing.
    ///
    /// Prefix matching is what makes the field usable as its name says: a
    /// block that writes several keys declares the folder once instead of
    /// enumerating every object path (or falling back to
    /// [`Allowlist::Any`]).
    pub fn allows_storage_folder(&self, resource: &str) -> bool {
        // Checked before the allowlist so the refusal holds for `Any` too:
        // an unnormalized `..` is never a resource anyone legitimately asks
        // for, and admitting it under `Any` would leave the WRAP
        // own-namespace rules reasoning about a path that escapes itself.
        if !crate::wrap::is_traversal_safe_path(resource) {
            return false;
        }
        let allow = match &self.storage_folders {
            Allowlist::None => return false,
            Allowlist::Any => return true,
            Allowlist::Only(set) => set,
        };
        allow
            .iter()
            .any(|entry| path_prefix_covers(entry, resource))
    }

    /// Whether outbound HTTP to `url` is permitted (SEC-06):
    /// - [`Allowlist::None`] → denied (network disabled).
    /// - [`Allowlist::Any`] → allowed.
    /// - [`Allowlist::Only`] → allowed iff `url` matches an allow entry by
    ///   exact scheme + host + port with a path prefix.
    pub fn allows_network_url(&self, url: &str) -> bool {
        let allow = match &self.network {
            Allowlist::None => return false,
            Allowlist::Any => return true,
            Allowlist::Only(set) => set,
        };
        // A raw `starts_with` prefix test does not bound the hostname, so
        // `https://a.com` would match `https://a.com.evil.net/...`. Parse both
        // and require an exact scheme + host + effective port, then a path
        // prefix — closing the hostname-boundary bypass (SEC-007 class) while
        // preserving the documented URL-prefix semantics for the path.
        // (The parameter shadows the `url` crate, so use `::url::Url`.)
        let Ok(target) = ::url::Url::parse(url) else {
            return false;
        };
        allow.iter().any(|allowed| {
            let Ok(pat) = ::url::Url::parse(allowed) else {
                return false;
            };
            pat.scheme() == target.scheme()
                && pat.host_str() == target.host_str()
                && pat.port_or_known_default() == target.port_or_known_default()
                && target.path().starts_with(pat.path())
        })
    }

    /// Whether this capability set permits operations on vector index `name`
    /// (matches `"*"` wildcard or an exact entry). Independent of
    /// `allows_collection`: a db-collection grant must not confer vector
    /// access, per the no-magic-mapping rule.
    pub fn allows_vector_index(&self, name: &str) -> bool {
        self.vector_indexes.allows(name)
    }

    /// Whether the block may read/write config key `key` (SEC-06):
    /// `None` → denied, `Any` → allowed, `Only(keys)` → allowed iff `key` is
    /// in the set (empty set denies everything).
    pub fn allows_config_key(&self, key: &str) -> bool {
        self.config.allows(key)
    }

    /// Check whether a call_block invocation to `target` is allowed.
    ///
    /// `"*"` = unrestricted. Empty set = no calls allowed.
    pub fn allows_call_block(&self, target: &str) -> bool {
        self.callable_blocks.allows(target)
    }

    /// Intersect two capability sets.
    ///
    /// Rules:
    /// - Booleans (`raw_sql`, `ddl`, `schema`, `crypto`): logical AND (both
    ///   must allow).
    /// - [`Allowlist`] fields (collections, network, config, vector_indexes,
    ///   callable_blocks): [`Allowlist::intersect`].
    /// - `storage_folders`: [`Allowlist::intersect_path_prefix`] — its entries
    ///   are path prefixes, so nesting on either side narrows rather than
    ///   denies.
    /// - HeaderPolicy readable / writable: intersection.
    /// - HeaderPolicy masked: UNION (denylists strengthen).
    pub fn intersect(&self, other: &Self) -> Self {
        Self {
            collections: self.collections.intersect(&other.collections),
            raw_sql: self.raw_sql && other.raw_sql,
            ddl: self.ddl && other.ddl,
            schema: self.schema && other.schema,
            storage_folders: self
                .storage_folders
                .intersect_path_prefix(&other.storage_folders),
            crypto: self.crypto && other.crypto,
            network: self.network.intersect(&other.network),
            config: self.config.intersect(&other.config),
            vector_indexes: self.vector_indexes.intersect(&other.vector_indexes),
            callable_blocks: self.callable_blocks.intersect(&other.callable_blocks),
            headers: HeaderPolicy {
                readable: intersect_vec(&self.headers.readable, &other.headers.readable),
                writable: intersect_vec(&self.headers.writable, &other.headers.writable),
                masked: union_vec(&self.headers.masked, &other.headers.masked),
            },
        }
    }

    /// Apply sparse config overrides onto this (declared) capability set.
    ///
    /// Fields the operator did not mention (`None` in the overrides) are
    /// preserved as declared. Fields the operator did mention are combined
    /// with declared using the intersection rules:
    ///
    /// - Booleans: logical AND (operator can only disable, not enable).
    /// - [`Allowlist`] fields: [`Allowlist::intersect`].
    /// - Vec allowlists (`network_allow` is gone; header vecs remain): intersection.
    /// - HeaderPolicy `readable`/`writable`: intersection.
    /// - HeaderPolicy `masked`: UNION (operator can add masking).
    ///
    /// # `None` vs `Some(empty)`
    ///
    /// These two override shapes have opposite meanings and the distinction is
    /// security-relevant:
    ///
    /// - `None` (field omitted) **preserves** the declared value.
    /// - `Some(<empty set/vec>)` is an explicit, non-wildcard allowlist that
    ///   intersects to **deny-all** for that field (`declared ∩ ∅ = ∅`). An
    ///   operator who writes `{ "collections": [] }` is narrowing the block to
    ///   *no* collections, not leaving it untouched.
    ///
    /// The `apply_overrides_empty_set_narrows_to_deny_all` unit test locks in
    /// this behavior. For `network`/`config` the ambiguity that SEC-06's #285
    /// compatibility patch guarded against is gone entirely: they are
    /// [`Allowlist`]s, so an override of `Only([])` intersects to `Only([])`
    /// (deny-all) with no "empty means any" to invert — no special-casing.
    pub fn apply_config_overrides(&self, o: &ConfigCapabilityOverrides) -> Self {
        let h = o.headers.as_ref();
        let effective = Self {
            collections: narrow(&self.collections, o.collections.as_ref(), |a, b| {
                a.intersect(b)
            }),
            raw_sql: narrow(&self.raw_sql, o.raw_sql.as_ref(), |a, b| *a && *b),
            ddl: narrow(&self.ddl, o.ddl.as_ref(), |a, b| *a && *b),
            schema: narrow(&self.schema, o.schema.as_ref(), |a, b| *a && *b),
            storage_folders: narrow(&self.storage_folders, o.storage_folders.as_ref(), |a, b| {
                a.intersect_path_prefix(b)
            }),
            crypto: narrow(&self.crypto, o.crypto.as_ref(), |a, b| *a && *b),
            network: narrow(&self.network, o.network.as_ref(), |a, b| a.intersect(b)),
            config: narrow(&self.config, o.config.as_ref(), |a, b| a.intersect(b)),
            vector_indexes: narrow(&self.vector_indexes, o.vector_indexes.as_ref(), |a, b| {
                a.intersect(b)
            }),
            callable_blocks: narrow(&self.callable_blocks, o.callable_blocks.as_ref(), |a, b| {
                a.intersect(b)
            }),
            headers: HeaderPolicy {
                readable: narrow(
                    &self.headers.readable,
                    h.and_then(|h| h.readable.as_ref()),
                    |a, b| intersect_vec(a, b),
                ),
                writable: narrow(
                    &self.headers.writable,
                    h.and_then(|h| h.writable.as_ref()),
                    |a, b| intersect_vec(a, b),
                ),
                masked: narrow(
                    &self.headers.masked,
                    h.and_then(|h| h.masked.as_ref()),
                    |a, b| union_vec(a, b),
                ),
            },
        };

        effective
    }
}

/// Apply a per-field combine rule only when the operator supplied an override.
///
/// `None` (field omitted from config) preserves `declared` exactly;
/// `Some(o)` combines via the same rule function [`BlockCapabilities::intersect`]
/// uses for that field — so each field's combination rule is named once here
/// and once in `intersect`, with the rule body living in a single function
/// ([`Allowlist::intersect`] / `intersect_vec` / `union_vec` / bool AND).
fn narrow<T: Clone, U>(declared: &T, ov: Option<&U>, combine: impl FnOnce(&T, &U) -> T) -> T {
    match ov {
        Some(o) => combine(declared, o),
        None => declared.clone(),
    }
}

/// Sparse capability overrides parsed from a block's config `capabilities`
/// subkey. `None` fields mean "keep whatever the block declared" — only
/// fields that are explicitly `Some` participate in narrowing.
///
/// This is the wire shape for operators. For example:
///
/// ```json
/// { "capabilities": { "network": false, "collections": ["users"] } }
/// ```
///
/// deserializes into `ConfigCapabilityOverrides { network: Some(false),
/// collections: Some({"users"}), ..everything else None }`. Fields the
/// operator didn't mention stay at the block's declared value after
/// `apply_config_overrides`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigCapabilityOverrides {
    /// Override for [`BlockCapabilities::collections`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collections: Option<Allowlist>,
    /// Override for [`BlockCapabilities::raw_sql`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_sql: Option<bool>,
    /// Override for [`BlockCapabilities::ddl`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ddl: Option<bool>,
    /// Override for [`BlockCapabilities::schema`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<bool>,
    /// Override for [`BlockCapabilities::storage_folders`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_folders: Option<Allowlist>,
    /// Override for [`BlockCapabilities::crypto`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crypto: Option<bool>,
    /// Override for [`BlockCapabilities::network`] (SEC-06). An operator writes
    /// `"None"` to deny, `"Any"` to allow all, or `{ "Only": ["https://x/"] }`
    /// to restrict — replacing the old `network` bool + `network_allow` list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Allowlist>,
    /// Override for [`BlockCapabilities::config`] (SEC-06). `"None"` / `"Any"` /
    /// `{ "Only": ["KEY"] }` — replacing the old `config` bool + `config_keys`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Allowlist>,
    /// Override for [`BlockCapabilities::vector_indexes`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_indexes: Option<Allowlist>,
    /// Override for [`BlockCapabilities::callable_blocks`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callable_blocks: Option<Allowlist>,
    /// Override for [`BlockCapabilities::headers`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<HeaderPolicyOverrides>,
}

/// Sparse header-policy overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HeaderPolicyOverrides {
    /// Override for [`HeaderPolicy::readable`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readable: Option<Vec<String>>,
    /// Override for [`HeaderPolicy::writable`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writable: Option<Vec<String>>,
    /// Override for [`HeaderPolicy::masked`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub masked: Option<Vec<String>>,
}

fn intersect_vec(a: &[String], b: &[String]) -> Vec<String> {
    a.iter()
        .filter(|x| b.iter().any(|y| y == *x))
        .cloned()
        .collect()
}

fn union_vec(a: &[String], b: &[String]) -> Vec<String> {
    let mut r: Vec<String> = a.to_vec();
    for v in b {
        if !r.iter().any(|x| x == v) {
            r.push(v.clone());
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_policy_defaults_empty() {
        let p = HeaderPolicy::default();
        assert!(p.readable.is_empty());
        assert!(p.writable.is_empty());
        assert!(p.masked.is_empty());
    }

    #[test]
    fn block_capabilities_default_has_empty_header_policy() {
        let caps = BlockCapabilities::default();
        assert!(caps.headers.readable.is_empty());
        assert!(caps.headers.writable.is_empty());
        assert!(caps.headers.masked.is_empty());
    }

    #[test]
    fn header_policy_roundtrips_through_json() {
        let p = HeaderPolicy {
            readable: vec!["authorization".into()],
            writable: vec!["set-cookie".into()],
            masked: vec!["x-internal".into()],
        };
        let j = serde_json::to_string(&p).unwrap();
        let back: HeaderPolicy = serde_json::from_str(&j).unwrap();
        assert_eq!(back.readable, p.readable);
        assert_eq!(back.writable, p.writable);
        assert_eq!(back.masked, p.masked);
    }

    /// Build an `Allowlist::Only` from a slice.
    fn only(items: &[&str]) -> Allowlist {
        Allowlist::Only(items.iter().map(|s| s.to_string()).collect())
    }

    fn caps_with_collections(items: &[&str]) -> BlockCapabilities {
        BlockCapabilities {
            collections: only(items),
            ..Default::default()
        }
    }

    fn caps_allowing(hosts: &[&str]) -> BlockCapabilities {
        BlockCapabilities {
            network: Allowlist::Only(hosts.iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        }
    }

    fn caps_with_storage_folders(items: &[&str]) -> BlockCapabilities {
        BlockCapabilities {
            storage_folders: only(items),
            ..Default::default()
        }
    }

    #[test]
    fn storage_folder_entry_admits_keys_beneath_it() {
        // The storage handler authorizes on `{folder}/{key}`, so a grant of a
        // FOLDER has to admit every object path under it — otherwise a block
        // that writes more than one key can only declare `Any`.
        let c = caps_with_storage_folders(&["site/jhg"]);
        assert!(c.allows_storage_folder("site/jhg/a.txt"));
        assert!(c.allows_storage_folder("site/jhg/sub/b"));
    }

    #[test]
    fn storage_folder_entry_admits_itself() {
        // Folder-level ops (list / create_folder / delete_folder) authorize on
        // the bare folder name, and an entry that names a single object path
        // must still admit exactly that object.
        let folder = caps_with_storage_folders(&["site/jhg"]);
        assert!(folder.allows_storage_folder("site/jhg"));
        let object = caps_with_storage_folders(&["site/jhg/a.txt"]);
        assert!(object.allows_storage_folder("site/jhg/a.txt"));
        assert!(
            !object.allows_storage_folder("site/jhg/a.txt.bak"),
            "an object-path entry must not leak to a longer sibling name"
        );
    }

    #[test]
    fn storage_folder_prefix_stops_at_a_separator() {
        // `site/jhg` must never admit `site/jhgx/...` — the prefix only counts
        // when the next character is the `/` path separator.
        let c = caps_with_storage_folders(&["site/jhg"]);
        assert!(!c.allows_storage_folder("site/jhgx/a.txt"));
        assert!(!c.allows_storage_folder("site/jhg-other"));
        assert!(!c.allows_storage_folder("other/jhg/a.txt"));
    }

    #[test]
    fn storage_folder_any_and_none_are_unchanged() {
        let open = BlockCapabilities {
            storage_folders: Allowlist::Any,
            ..Default::default()
        };
        assert!(open.allows_storage_folder("anything/at/all"));
        let closed = BlockCapabilities {
            storage_folders: Allowlist::None,
            ..Default::default()
        };
        assert!(!closed.allows_storage_folder("site/jhg/a.txt"));
        let empty = BlockCapabilities {
            storage_folders: Allowlist::Only(BTreeSet::new()),
            ..Default::default()
        };
        assert!(!empty.allows_storage_folder("site/jhg/a.txt"));
    }

    // --- C1: traversal shapes and degenerate entries -----------------------

    /// The C1 exploit: a guest granted the folder `site/jhg` asks for key
    /// `../../other/secret`, so the handler authorizes on
    /// `site/jhg/../../other/secret` — textually beneath the grant, but naming
    /// a folder the guest was never given.
    #[test]
    fn storage_folder_refuses_parent_traversal_beneath_a_grant() {
        let c = caps_with_storage_folders(&["site/jhg"]);
        assert!(!c.allows_storage_folder("site/jhg/../../other/secret"));
        assert!(!c.allows_storage_folder("site/jhg/../other"));
        assert!(!c.allows_storage_folder("site/jhg/.."));
    }

    #[test]
    fn storage_folder_refuses_dot_and_empty_segments() {
        let c = caps_with_storage_folders(&["site/jhg"]);
        assert!(!c.allows_storage_folder("site/jhg/./a.txt"));
        assert!(!c.allows_storage_folder("site/jhg//a.txt"));
        assert!(!c.allows_storage_folder("site/jhg/"));
        assert!(!c.allows_storage_folder(""));
        // A leading separator is an empty first segment.
        assert!(!c.allows_storage_folder("/site/jhg/a.txt"));
    }

    /// The refusal is a property of the RESOURCE, not of the allowlist: an
    /// unrestricted `Any` must not admit a traversal shape either, so no
    /// downstream rule (WRAP's own-namespace self-admit) ever reasons about a
    /// path that escapes itself.
    #[test]
    fn storage_folder_refuses_traversal_even_under_any() {
        let open = BlockCapabilities {
            storage_folders: Allowlist::Any,
            ..Default::default()
        };
        assert!(open.allows_storage_folder("site/jhg/a.txt"));
        assert!(!open.allows_storage_folder("site/jhg/../../other/secret"));
        assert!(!open.allows_storage_folder(""));
    }

    /// Degenerate `Only` entries match nothing rather than prefix-matching by
    /// accident.
    #[test]
    fn degenerate_storage_folder_entries_match_nothing() {
        let empty_entry = caps_with_storage_folders(&[""]);
        assert!(!empty_entry.allows_storage_folder("site/jhg/a.txt"));
        assert!(!empty_entry.allows_storage_folder("site"));

        let trailing_slash = caps_with_storage_folders(&["site/jhg/"]);
        assert!(!trailing_slash.allows_storage_folder("site/jhg/a.txt"));
        assert!(!trailing_slash.allows_storage_folder("site/jhg"));

        // The correctly-spelled entry still works — the rule only refuses the
        // degenerate spellings.
        let ok = caps_with_storage_folders(&["site/jhg"]);
        assert!(ok.allows_storage_folder("site/jhg/a.txt"));
    }

    // --- M4: prefix-aware narrowing for storage_folders ---------------------

    #[test]
    fn storage_folder_intersect_keeps_the_narrower_nested_entry() {
        // Override nested under the declaration: the operator narrowed.
        let declared = only(&["site"]);
        let over = only(&["site/jhg"]);
        assert_eq!(declared.intersect_path_prefix(&over), only(&["site/jhg"]));
        // Declaration nested under the override: the block is already
        // narrower and must survive (the operator cannot widen it).
        assert_eq!(over.intersect_path_prefix(&declared), only(&["site/jhg"]));
        // A plain set intersection would have denied both cases outright.
        assert_eq!(declared.intersect(&over), only(&[]));
    }

    #[test]
    fn storage_folder_intersect_drops_unrelated_entries() {
        let declared = only(&["site/jhg", "shared"]);
        let over = only(&["site/jhg/sub", "other"]);
        assert_eq!(
            declared.intersect_path_prefix(&over),
            only(&["site/jhg/sub"]),
            "only entries with a covering relationship survive"
        );
        // `Any`/`None` behave exactly as for the exact-match intersection.
        assert_eq!(
            Allowlist::Any.intersect_path_prefix(&declared),
            declared.clone()
        );
        assert_eq!(
            Allowlist::None.intersect_path_prefix(&declared),
            Allowlist::None
        );
    }

    /// Narrowing through the capability set (not just the allowlist) uses the
    /// prefix rule, so the effective set still admits keys under the narrower
    /// folder.
    #[test]
    fn caps_intersect_narrows_storage_folders_by_prefix() {
        let declared = caps_with_storage_folders(&["site"]);
        let over = caps_with_storage_folders(&["site/jhg"]);
        let eff = declared.intersect(&over);
        assert!(eff.allows_storage_folder("site/jhg/a.txt"));
        assert!(!eff.allows_storage_folder("site/other/a.txt"));
    }

    // --- I1: the schema capability is independent of ddl --------------------

    #[test]
    fn schema_capability_defaults_closed_and_opens_only_with_unrestricted() {
        assert!(!BlockCapabilities::default().schema);
        assert!(!BlockCapabilities::none().schema);
        assert!(BlockCapabilities::unrestricted().schema);
    }

    #[test]
    fn schema_and_ddl_narrow_independently() {
        let declared = BlockCapabilities {
            schema: true,
            ddl: false,
            ..Default::default()
        };
        // Intersection is a plain AND per field — `ddl` cannot be borrowed
        // from `schema` or vice versa.
        let eff = declared.intersect(&BlockCapabilities::unrestricted());
        assert!(eff.schema);
        assert!(!eff.ddl);

        // An operator can drop `schema` while leaving `ddl` alone.
        let narrowed = declared.apply_config_overrides(&ConfigCapabilityOverrides {
            schema: Some(false),
            ..Default::default()
        });
        assert!(!narrowed.schema);

        // ...and cannot grant `schema` a block never declared.
        let widened =
            BlockCapabilities::none().apply_config_overrides(&ConfigCapabilityOverrides {
                schema: Some(true),
                ..Default::default()
            });
        assert!(!widened.schema);
    }

    #[test]
    fn schema_override_deserializes_from_operator_json() {
        let o: ConfigCapabilityOverrides =
            serde_json::from_str(r#"{"schema": false, "ddl": true}"#).unwrap();
        assert_eq!(o.schema, Some(false));
        assert_eq!(o.ddl, Some(true));
        // Omitted stays `None` = "keep whatever the block declared".
        let o2: ConfigCapabilityOverrides = serde_json::from_str(r#"{"ddl": true}"#).unwrap();
        assert_eq!(o2.schema, None);
    }

    #[test]
    fn network_allow_rejects_hostname_suffix_bypass() {
        // The F8 bug: `https://a.com` must NOT match `https://a.com.evil.net`.
        let c = caps_allowing(&["https://a.com"]);
        assert!(!c.allows_network_url("https://a.com.evil.net/steal"));
        let c2 = caps_allowing(&["https://a.com/"]);
        assert!(!c2.allows_network_url("https://a.com.evil.net/steal"));
    }

    #[test]
    fn network_allow_permits_exact_host_and_subpaths() {
        let c = caps_allowing(&["https://a.com/"]);
        assert!(c.allows_network_url("https://a.com/"));
        assert!(c.allows_network_url("https://a.com/foo/bar"));
        // Path prefix still scopes: an allow of /v1/ must not permit /v2.
        let stripe = caps_allowing(&["https://api.stripe.com/v1/"]);
        assert!(stripe.allows_network_url("https://api.stripe.com/v1/charges"));
        assert!(!stripe.allows_network_url("https://api.stripe.com/v2/refunds"));
    }

    #[test]
    fn network_allow_requires_exact_scheme_and_port() {
        let c = caps_allowing(&["https://a.com/"]);
        assert!(!c.allows_network_url("http://a.com/"), "scheme must match");
        assert!(
            !c.allows_network_url("https://a.com:8443/"),
            "port must match"
        );
    }

    #[test]
    fn network_any_is_unrestricted_none_is_denied() {
        let open = BlockCapabilities {
            network: Allowlist::Any,
            ..Default::default()
        };
        assert!(open.allows_network_url("https://anything.example/"));
        // Allowlist::None → always denied even if url would otherwise match.
        let mut off = caps_allowing(&["https://a.com/"]);
        off.network = Allowlist::None;
        assert!(!off.allows_network_url("https://a.com/"));
        // SEC-06: an empty `Only` is deny-all, NOT "any" — the old empty-Vec
        // sentinel is gone.
        let empty = BlockCapabilities {
            network: Allowlist::Only(BTreeSet::new()),
            ..Default::default()
        };
        assert!(!empty.allows_network_url("https://anything.example/"));
    }

    #[test]
    fn network_allow_rejects_malformed_url() {
        let c = caps_allowing(&["https://a.com/"]);
        assert!(!c.allows_network_url("not a url"));
    }

    #[test]
    fn network_allow_userinfo_does_not_confuse_host() {
        // `a.com` here is userinfo, not the host — host is evil.com → deny.
        let c = caps_allowing(&["https://a.com/"]);
        assert!(!c.allows_network_url("https://a.com@evil.com/steal"));
    }

    #[test]
    fn network_allow_path_traversal_is_normalized_before_prefix_check() {
        // `/v1/../v2/secret` normalizes to `/v2/secret`, which is not under /v1/.
        let c = caps_allowing(&["https://api.stripe.com/v1/"]);
        assert!(!c.allows_network_url("https://api.stripe.com/v1/../v2/secret"));
    }

    #[test]
    fn network_allow_ignores_query_and_fragment() {
        // Query string / fragment must not defeat a legitimate path-prefix match.
        let c = caps_allowing(&["https://a.com/"]);
        assert!(c.allows_network_url("https://a.com/path?q=1#frag"));
    }

    #[test]
    fn intersect_booleans_and() {
        let a = BlockCapabilities {
            crypto: true,
            network: Allowlist::Any,
            raw_sql: false,
            ..Default::default()
        };
        let b = BlockCapabilities {
            crypto: true,
            network: Allowlist::None,
            raw_sql: true,
            ..Default::default()
        };
        let r = a.intersect(&b);
        assert!(r.crypto);
        assert_eq!(r.network, Allowlist::None); // Any ∩ None = None
        assert!(!r.raw_sql);
    }

    #[test]
    fn allowlist_intersect_rules() {
        use Allowlist::*;
        let only_ab = Only(["a", "b"].iter().map(|s| s.to_string()).collect());
        let only_bc = Only(["b", "c"].iter().map(|s| s.to_string()).collect());
        // None dominates.
        assert_eq!(None.intersect(&Any), None);
        assert_eq!(Any.intersect(&None), None);
        // Any yields the other side.
        assert_eq!(Any.intersect(&only_ab), only_ab);
        assert_eq!(only_ab.intersect(&Any), only_ab);
        // Only ∩ Only = set intersection.
        assert_eq!(
            only_ab.intersect(&only_bc),
            Only(["b"].iter().map(|s| s.to_string()).collect())
        );
        // Disjoint Only ∩ Only = empty Only = deny-all (unambiguous).
        let only_x = Only(["x"].iter().map(|s| s.to_string()).collect());
        assert_eq!(only_ab.intersect(&only_x), Only(BTreeSet::new()));
    }

    #[test]
    fn intersect_collections_set_intersection() {
        let a = caps_with_collections(&["a", "b", "c"]);
        let b = caps_with_collections(&["b", "c", "d"]);
        let r = a.intersect(&b);
        assert_eq!(r.collections, only(&["b", "c"]));
    }

    #[test]
    fn intersect_any_yields_other_side() {
        // `Any` (was the "*" sentinel) ∩ a specific set → that set.
        let a = BlockCapabilities {
            collections: Allowlist::Any,
            ..Default::default()
        };
        let b = caps_with_collections(&["users"]);
        let r = a.intersect(&b);
        assert_eq!(r.collections, only(&["users"]));
    }

    #[test]
    fn intersect_any_both_yields_any() {
        let a = BlockCapabilities {
            collections: Allowlist::Any,
            ..Default::default()
        };
        let b = BlockCapabilities {
            collections: Allowlist::Any,
            ..Default::default()
        };
        let r = a.intersect(&b);
        assert_eq!(r.collections, Allowlist::Any);
    }

    #[test]
    fn intersect_network_only_set_intersection() {
        let a = BlockCapabilities {
            network: Allowlist::Only(
                ["https://a.com/", "https://b.com/"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
            ..Default::default()
        };
        let b = BlockCapabilities {
            network: Allowlist::Only(
                ["https://b.com/", "https://c.com/"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
            ..Default::default()
        };
        let r = a.intersect(&b);
        assert_eq!(
            r.network,
            Allowlist::Only(["https://b.com/"].iter().map(|s| s.to_string()).collect())
        );
    }

    #[test]
    fn intersect_header_policy_readable_intersects() {
        let a = BlockCapabilities {
            headers: HeaderPolicy {
                readable: vec!["authorization".into(), "cookie".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let b = BlockCapabilities {
            headers: HeaderPolicy {
                readable: vec!["cookie".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let r = a.intersect(&b);
        assert_eq!(r.headers.readable, vec!["cookie".to_string()]);
    }

    #[test]
    fn intersect_header_policy_masked_unions() {
        let a = BlockCapabilities {
            headers: HeaderPolicy {
                masked: vec!["x-a".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let b = BlockCapabilities {
            headers: HeaderPolicy {
                masked: vec!["x-b".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let r = a.intersect(&b);
        let mut got = r.headers.masked;
        got.sort();
        assert_eq!(got, vec!["x-a".to_string(), "x-b".to_string()]);
    }

    #[test]
    fn apply_overrides_empty_keeps_declared() {
        let declared = BlockCapabilities {
            crypto: true,
            network: Allowlist::Any,
            collections: only(&["users"]),
            callable_blocks: only(&["wafer-run/crypto"]),
            ..Default::default()
        };
        let overrides = ConfigCapabilityOverrides::default();
        let eff = declared.apply_config_overrides(&overrides);

        // All declared fields preserved — nothing silently wiped.
        assert!(eff.crypto);
        assert_eq!(eff.network, Allowlist::Any);
        assert!(eff.collections.allows("users"));
        assert!(eff.callable_blocks.allows("wafer-run/crypto"));
    }

    // SEC-06: with `network`/`config` as `Allowlist`, an operator override of
    // `Only([])` (or `None`) is unambiguous deny-all — no empty-container
    // sentinel to invert. Assert the *enforcement* outcome, not just the value.
    #[test]
    fn empty_network_only_override_denies_all_urls_at_enforcement() {
        let declared = BlockCapabilities {
            network: Allowlist::Only(["https://a.com/"].iter().map(|s| s.to_string()).collect()),
            ..BlockCapabilities::none()
        };
        let over = ConfigCapabilityOverrides {
            network: Some(Allowlist::Only(BTreeSet::new())),
            ..Default::default()
        };
        let eff = declared.apply_config_overrides(&over);
        assert_eq!(
            eff.network,
            Allowlist::Only(BTreeSet::new()),
            "an explicit empty Only override intersects to deny-all"
        );
        assert!(!eff.allows_network_url("https://a.com/"));
        assert!(!eff.allows_network_url("https://anything.example/"));
    }

    #[test]
    fn none_config_override_denies_all_keys_at_enforcement() {
        let declared = BlockCapabilities {
            config: Allowlist::Only(
                ["ACME__WIDGET__KEY"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
            ..BlockCapabilities::none()
        };
        let over = ConfigCapabilityOverrides {
            config: Some(Allowlist::None),
            ..Default::default()
        };
        let eff = declared.apply_config_overrides(&over);
        assert_eq!(eff.config, Allowlist::None);
        assert!(!eff.allows_config_key("ACME__WIDGET__KEY"));
    }

    #[test]
    fn apply_overrides_empty_set_narrows_to_deny_all() {
        // Contrast with `apply_overrides_empty_keeps_declared`: there the
        // override fields are `None` (omitted) and the declared values are
        // preserved. Here the operator supplies explicit *empty* allowlists,
        // which is a non-wildcard allowlist that intersects to deny-all.
        let declared = BlockCapabilities {
            collections: only(&["users", "sessions"]),
            storage_folders: only(&["uploads"]),
            network: Allowlist::Only(["https://a.com/"].iter().map(|s| s.to_string()).collect()),
            callable_blocks: only(&["wafer-run/crypto"]),
            config: Allowlist::Only(
                ["ACME__WIDGET__KEY"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
            ..Default::default()
        };
        let overrides = ConfigCapabilityOverrides {
            collections: Some(Allowlist::Only(BTreeSet::new())),
            storage_folders: Some(Allowlist::Only(BTreeSet::new())),
            network: Some(Allowlist::Only(BTreeSet::new())),
            callable_blocks: Some(Allowlist::Only(BTreeSet::new())),
            config: Some(Allowlist::Only(BTreeSet::new())),
            ..Default::default()
        };
        let eff = declared.apply_config_overrides(&overrides);

        // Every explicitly-emptied allowlist narrows to deny-all (∅).
        assert!(
            eff.collections == Allowlist::Only(BTreeSet::new()),
            "empty override → no collections"
        );
        assert!(
            eff.storage_folders == Allowlist::Only(BTreeSet::new()),
            "empty override → no storage folders"
        );
        assert_eq!(
            eff.network,
            Allowlist::Only(BTreeSet::new()),
            "empty Only override → deny-all network"
        );
        assert!(
            eff.callable_blocks == Allowlist::Only(BTreeSet::new()),
            "empty override → no callable blocks"
        );
        assert_eq!(
            eff.config,
            Allowlist::Only(BTreeSet::new()),
            "empty Only override → deny-all config"
        );
    }

    #[test]
    fn apply_overrides_partial_preserves_untouched_fields() {
        let declared = BlockCapabilities {
            crypto: true,
            network: Allowlist::Only(
                ["https://a.com/", "https://b.com/"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
            collections: only(&["users", "sessions"]),
            callable_blocks: only(&["wafer-run/crypto"]),
            ..Default::default()
        };
        // Operator narrows ONLY network.
        let overrides = ConfigCapabilityOverrides {
            network: Some(Allowlist::Only(
                ["https://a.com/"].iter().map(|s| s.to_string()).collect(),
            )),
            ..Default::default()
        };
        let eff = declared.apply_config_overrides(&overrides);

        // Explicit narrowing applied (Only(a,b) ∩ Only(a) = Only(a)).
        assert_eq!(
            eff.network,
            Allowlist::Only(["https://a.com/"].iter().map(|s| s.to_string()).collect())
        );
        // Everything else preserved.
        assert!(eff.crypto);
        assert!(eff.collections.allows("users"));
        assert!(eff.collections.allows("sessions"));
        assert!(eff.callable_blocks.allows("wafer-run/crypto"));
    }

    #[test]
    fn apply_overrides_narrowing() {
        let declared = BlockCapabilities {
            crypto: true,
            network: Allowlist::Any,
            ..Default::default()
        };
        let overrides = ConfigCapabilityOverrides {
            network: Some(Allowlist::None),
            ..Default::default()
        };
        let eff = declared.apply_config_overrides(&overrides);
        assert!(eff.crypto, "crypto untouched");
        assert_eq!(eff.network, Allowlist::None, "network narrowed to None");
    }

    #[test]
    fn apply_overrides_cannot_widen() {
        let declared = BlockCapabilities {
            network: Allowlist::None,
            ..Default::default()
        };
        let overrides = ConfigCapabilityOverrides {
            network: Some(Allowlist::Any),
            ..Default::default()
        };
        let eff = declared.apply_config_overrides(&overrides);
        // Declared denied; config attempts to widen; declared wins (None ∩ Any = None).
        assert_eq!(eff.network, Allowlist::None);
    }

    #[test]
    fn apply_overrides_header_policy_partial() {
        let declared = BlockCapabilities {
            headers: HeaderPolicy {
                readable: vec!["authorization".into(), "cookie".into()],
                writable: vec!["set-cookie".into()],
                masked: vec![],
            },
            ..Default::default()
        };
        let overrides = ConfigCapabilityOverrides {
            headers: Some(HeaderPolicyOverrides {
                // Only narrow readable; writable untouched.
                readable: Some(vec!["authorization".into()]),
                writable: None,
                masked: None,
            }),
            ..Default::default()
        };
        let eff = declared.apply_config_overrides(&overrides);
        assert_eq!(eff.headers.readable, vec!["authorization".to_string()]);
        // Untouched writable preserved.
        assert_eq!(eff.headers.writable, vec!["set-cookie".to_string()]);
    }

    #[test]
    fn apply_overrides_partial_json_roundtrip() {
        // Verify the JSON wire format: absent fields → None → preserved; a
        // present `network` override deserializes into an `Allowlist` and
        // narrows. (SEC-06: the override wire shape is the enum, e.g.
        // `{"Only": [...]}`, not the old `network` bool + `network_allow` list.)
        let declared = BlockCapabilities {
            crypto: true,
            network: Allowlist::Any,
            collections: only(&["users"]),
            ..Default::default()
        };
        let json = serde_json::json!({ "network": { "Only": ["https://a.com/"] } });
        let overrides: ConfigCapabilityOverrides = serde_json::from_value(json).unwrap();
        let eff = declared.apply_config_overrides(&overrides);
        assert!(eff.crypto);
        assert!(eff.collections.allows("users")); // absent override → preserved
        assert_eq!(
            eff.network,
            Allowlist::Only(["https://a.com/"].iter().map(|s| s.to_string()).collect())
        );
    }

    #[test]
    fn allows_vector_index_wildcard_and_exact() {
        let unrestricted = BlockCapabilities::unrestricted();
        assert!(unrestricted.allows_vector_index("my_org__vector__docs"));

        let none = BlockCapabilities::none();
        assert!(!none.allows_vector_index("my_org__vector__docs"));

        let scoped = BlockCapabilities {
            vector_indexes: only(&["my_org__vector__docs"]),
            ..Default::default()
        };
        assert!(scoped.allows_vector_index("my_org__vector__docs"));
        assert!(!scoped.allows_vector_index("my_org__vector__other"));
    }

    #[test]
    fn intersect_vector_indexes_set_intersection() {
        let a = BlockCapabilities {
            vector_indexes: only(&["a", "b"]),
            ..Default::default()
        };
        let b = BlockCapabilities {
            vector_indexes: only(&["b", "c"]),
            ..Default::default()
        };
        let r = a.intersect(&b);
        assert_eq!(r.vector_indexes, only(&["b"]));
    }
}
