//! Block capability declarations and enforcement policy.
//!
//! See the wafer-site page "Block capabilities" for the high-level
//! model. TL;DR:
//!
//! - Blocks declare required capabilities in `BlockInfo::capabilities`.
//! - Operators narrow via a `capabilities` subkey in block config.
//! - The runtime intersects declared ∩ config and enforces on WASM blocks.
//! - Native blocks' declarations are documentation-only.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

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
    /// Allowed database collections. "*" = all, empty = none.
    #[serde(default)]
    pub collections: HashSet<String>,
    /// Can use query_raw/exec_raw.
    #[serde(default)]
    pub raw_sql: bool,
    /// Can issue DDL via `db::ddl()` (CREATE TABLE / INDEX / DROP / etc).
    /// Convention: blocks only DDL their own (`{org}__{block}__*`) tables; this
    /// is enforced by code review + the WRAP-grant audit script, not by parsing
    /// SQL. Default `false` to keep `none()` fully sandboxed; native blocks get
    /// `true` via `unrestricted()`.
    #[serde(default)]
    pub ddl: bool,
    /// Allowed storage folders. "*" = all, empty = none.
    #[serde(default)]
    pub storage_folders: HashSet<String>,
    /// Can use crypto service.
    #[serde(default)]
    pub crypto: bool,
    /// Can use network service.
    #[serde(default)]
    pub network: bool,
    /// URL prefix allowlist for network requests. Empty = any (if network=true).
    #[serde(default)]
    pub network_allow: Vec<String>,
    /// Can use config service.
    #[serde(default)]
    pub config: bool,
    /// Allowed config key patterns.
    #[serde(default)]
    pub config_keys: HashSet<String>,
    /// Blocks that may be called via `call_block()`. Empty = unrestricted.
    #[serde(default)]
    pub callable_blocks: HashSet<String>,
    /// Per-header read/write/mask policy.
    #[serde(default)]
    pub headers: HeaderPolicy,
}

impl BlockCapabilities {
    /// Unrestricted capabilities — used by native Rust blocks. All services
    /// allowed, all wildcards set, no allowlists enforced.
    pub fn unrestricted() -> Self {
        Self {
            collections: {
                let mut s = HashSet::new();
                s.insert("*".to_string());
                s
            },
            raw_sql: true,
            ddl: true,
            storage_folders: {
                let mut s = HashSet::new();
                s.insert("*".to_string());
                s
            },
            crypto: true,
            network: true,
            network_allow: Vec::new(),
            config: true,
            config_keys: HashSet::new(),
            callable_blocks: {
                let mut s = HashSet::new();
                s.insert("*".to_string());
                s
            },
            headers: HeaderPolicy::default(),
        }
    }

    /// No capabilities — completely sandboxed default for untrusted WASM blocks.
    pub fn none() -> Self {
        Self {
            collections: HashSet::new(),
            raw_sql: false,
            ddl: false,
            storage_folders: HashSet::new(),
            crypto: false,
            network: false,
            network_allow: Vec::new(),
            config: false,
            config_keys: HashSet::new(),
            callable_blocks: HashSet::new(), // empty = no calls allowed
            headers: HeaderPolicy::default(),
        }
    }

    /// Whether this capability set permits operations against `collection`
    /// (matches `"*"` wildcard or an exact entry).
    pub fn allows_collection(&self, collection: &str) -> bool {
        self.collections.contains("*") || self.collections.contains(collection)
    }

    /// Whether this capability set permits operations on `folder` in the
    /// storage service (matches `"*"` wildcard or an exact entry).
    pub fn allows_storage_folder(&self, folder: &str) -> bool {
        self.storage_folders.contains("*") || self.storage_folders.contains(folder)
    }

    /// Whether outbound HTTP to `url` is permitted (network enabled, and URL
    /// matches one of `network_allow` prefixes, or `network_allow` is empty).
    pub fn allows_network_url(&self, url: &str) -> bool {
        if !self.network {
            return false;
        }
        if self.network_allow.is_empty() {
            return true;
        }
        self.network_allow.iter().any(|allowed| {
            if url.starts_with(allowed) {
                return true;
            }
            if let Some(stripped) = allowed.strip_suffix('/') {
                url.starts_with(stripped) && url.as_bytes().get(stripped.len()) == Some(&b'/')
            } else {
                false
            }
        })
    }

    /// Whether the block may read/write config key `key` (empty allowlist
    /// means no restriction).
    pub fn allows_config_key(&self, key: &str) -> bool {
        if self.config_keys.is_empty() {
            return true;
        }
        self.config_keys.contains(key)
    }

    /// Check whether a call_block invocation to `target` is allowed.
    ///
    /// `"*"` = unrestricted. Empty set = no calls allowed.
    pub fn allows_call_block(&self, target: &str) -> bool {
        self.callable_blocks.contains("*") || self.callable_blocks.contains(target)
    }

    /// Intersect two capability sets.
    ///
    /// Rules:
    /// - Booleans: logical AND (both must allow).
    /// - HashSet allowlists (collections, storage_folders, config_keys, callable_blocks):
    ///   set intersection. Wildcard sentinel `"*"` on one side yields the other side.
    /// - Vec allowlist (network_allow): set intersection, preserves self's order.
    /// - HeaderPolicy readable / writable: intersection.
    /// - HeaderPolicy masked: UNION (denylists strengthen).
    pub fn intersect(&self, other: &Self) -> Self {
        Self {
            collections: intersect_wildcard_set(&self.collections, &other.collections),
            raw_sql: self.raw_sql && other.raw_sql,
            ddl: self.ddl && other.ddl,
            storage_folders: intersect_wildcard_set(&self.storage_folders, &other.storage_folders),
            crypto: self.crypto && other.crypto,
            network: self.network && other.network,
            network_allow: intersect_vec(&self.network_allow, &other.network_allow),
            config: self.config && other.config,
            config_keys: intersect_wildcard_set(&self.config_keys, &other.config_keys),
            callable_blocks: intersect_wildcard_set(&self.callable_blocks, &other.callable_blocks),
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
    /// - HashSet allowlists: set intersection (with `"*"` wildcard).
    /// - Vec allowlists: set intersection.
    /// - HeaderPolicy `readable`/`writable`: intersection.
    /// - HeaderPolicy `masked`: UNION (operator can add masking).
    pub fn apply_config_overrides(&self, o: &ConfigCapabilityOverrides) -> Self {
        let headers = match &o.headers {
            Some(h) => HeaderPolicy {
                readable: match &h.readable {
                    Some(r) => intersect_vec(&self.headers.readable, r),
                    None => self.headers.readable.clone(),
                },
                writable: match &h.writable {
                    Some(w) => intersect_vec(&self.headers.writable, w),
                    None => self.headers.writable.clone(),
                },
                masked: match &h.masked {
                    Some(m) => union_vec(&self.headers.masked, m),
                    None => self.headers.masked.clone(),
                },
            },
            None => self.headers.clone(),
        };

        Self {
            collections: match &o.collections {
                Some(c) => intersect_wildcard_set(&self.collections, c),
                None => self.collections.clone(),
            },
            raw_sql: match o.raw_sql {
                Some(r) => self.raw_sql && r,
                None => self.raw_sql,
            },
            ddl: match o.ddl {
                Some(d) => self.ddl && d,
                None => self.ddl,
            },
            storage_folders: match &o.storage_folders {
                Some(s) => intersect_wildcard_set(&self.storage_folders, s),
                None => self.storage_folders.clone(),
            },
            crypto: match o.crypto {
                Some(c) => self.crypto && c,
                None => self.crypto,
            },
            network: match o.network {
                Some(n) => self.network && n,
                None => self.network,
            },
            network_allow: match &o.network_allow {
                Some(n) => intersect_vec(&self.network_allow, n),
                None => self.network_allow.clone(),
            },
            config: match o.config {
                Some(c) => self.config && c,
                None => self.config,
            },
            config_keys: match &o.config_keys {
                Some(c) => intersect_wildcard_set(&self.config_keys, c),
                None => self.config_keys.clone(),
            },
            callable_blocks: match &o.callable_blocks {
                Some(c) => intersect_wildcard_set(&self.callable_blocks, c),
                None => self.callable_blocks.clone(),
            },
            headers,
        }
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
    pub collections: Option<HashSet<String>>,
    /// Override for [`BlockCapabilities::raw_sql`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_sql: Option<bool>,
    /// Override for [`BlockCapabilities::ddl`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ddl: Option<bool>,
    /// Override for [`BlockCapabilities::storage_folders`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_folders: Option<HashSet<String>>,
    /// Override for [`BlockCapabilities::crypto`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crypto: Option<bool>,
    /// Override for [`BlockCapabilities::network`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<bool>,
    /// Override for [`BlockCapabilities::network_allow`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_allow: Option<Vec<String>>,
    /// Override for [`BlockCapabilities::config`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<bool>,
    /// Override for [`BlockCapabilities::config_keys`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_keys: Option<HashSet<String>>,
    /// Override for [`BlockCapabilities::callable_blocks`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callable_blocks: Option<HashSet<String>>,
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

fn intersect_wildcard_set(a: &HashSet<String>, b: &HashSet<String>) -> HashSet<String> {
    let a_any = a.contains("*");
    let b_any = b.contains("*");
    match (a_any, b_any) {
        (true, true) => {
            let mut r = HashSet::new();
            r.insert("*".to_string());
            r
        }
        (true, false) => b.clone(),
        (false, true) => a.clone(),
        (false, false) => a.intersection(b).cloned().collect(),
    }
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

    use std::collections::HashSet;

    fn caps_with_collections(items: &[&str]) -> BlockCapabilities {
        BlockCapabilities {
            collections: items.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn intersect_booleans_and() {
        let a = BlockCapabilities {
            crypto: true,
            network: true,
            raw_sql: false,
            ..Default::default()
        };
        let b = BlockCapabilities {
            crypto: true,
            network: false,
            raw_sql: true,
            ..Default::default()
        };
        let r = a.intersect(&b);
        assert!(r.crypto);
        assert!(!r.network);
        assert!(!r.raw_sql);
    }

    #[test]
    fn intersect_collections_set_intersection() {
        let a = caps_with_collections(&["a", "b", "c"]);
        let b = caps_with_collections(&["b", "c", "d"]);
        let r = a.intersect(&b);
        let expected: HashSet<String> = ["b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(r.collections, expected);
    }

    #[test]
    fn intersect_wildcard_sentinel_left_yields_right() {
        let a = caps_with_collections(&["*"]);
        let b = caps_with_collections(&["users"]);
        let r = a.intersect(&b);
        let expected: HashSet<String> = ["users"].iter().map(|s| s.to_string()).collect();
        assert_eq!(r.collections, expected);
    }

    #[test]
    fn intersect_wildcard_sentinel_both_yields_wildcard() {
        let a = caps_with_collections(&["*"]);
        let b = caps_with_collections(&["*"]);
        let r = a.intersect(&b);
        let expected: HashSet<String> = ["*"].iter().map(|s| s.to_string()).collect();
        assert_eq!(r.collections, expected);
    }

    #[test]
    fn intersect_network_allow_vec_intersection() {
        let a = BlockCapabilities {
            network_allow: vec!["https://a.com/".into(), "https://b.com/".into()],
            ..Default::default()
        };
        let b = BlockCapabilities {
            network_allow: vec!["https://b.com/".into(), "https://c.com/".into()],
            ..Default::default()
        };
        let r = a.intersect(&b);
        assert_eq!(r.network_allow, vec!["https://b.com/".to_string()]);
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
            network: true,
            collections: ["users"].iter().map(|s| s.to_string()).collect(),
            callable_blocks: ["wafer-run/crypto"].iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let overrides = ConfigCapabilityOverrides::default();
        let eff = declared.apply_config_overrides(&overrides);

        // All declared fields preserved — nothing silently wiped.
        assert!(eff.crypto);
        assert!(eff.network);
        assert!(eff.collections.contains("users"));
        assert!(eff.callable_blocks.contains("wafer-run/crypto"));
    }

    #[test]
    fn apply_overrides_partial_preserves_untouched_fields() {
        let declared = BlockCapabilities {
            crypto: true,
            network: true,
            network_allow: vec!["https://a.com/".into(), "https://b.com/".into()],
            collections: ["users", "sessions"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            callable_blocks: ["wafer-run/crypto"].iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        // Operator narrows ONLY network_allow.
        let overrides = ConfigCapabilityOverrides {
            network_allow: Some(vec!["https://a.com/".into()]),
            ..Default::default()
        };
        let eff = declared.apply_config_overrides(&overrides);

        // Explicit narrowing applied.
        assert_eq!(eff.network_allow, vec!["https://a.com/".to_string()]);
        // Everything else preserved.
        assert!(eff.crypto);
        assert!(eff.network);
        assert!(eff.collections.contains("users"));
        assert!(eff.collections.contains("sessions"));
        assert!(eff.callable_blocks.contains("wafer-run/crypto"));
    }

    #[test]
    fn apply_overrides_bool_narrowing() {
        let declared = BlockCapabilities {
            crypto: true,
            network: true,
            ..Default::default()
        };
        let overrides = ConfigCapabilityOverrides {
            network: Some(false),
            ..Default::default()
        };
        let eff = declared.apply_config_overrides(&overrides);
        assert!(eff.crypto, "crypto untouched");
        assert!(!eff.network, "network narrowed to false");
    }

    #[test]
    fn apply_overrides_bool_cannot_widen() {
        let declared = BlockCapabilities {
            network: false,
            ..Default::default()
        };
        let overrides = ConfigCapabilityOverrides {
            network: Some(true),
            ..Default::default()
        };
        let eff = declared.apply_config_overrides(&overrides);
        // Declared denied; config attempts to widen; declared wins.
        assert!(!eff.network);
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
        // Verify the JSON wire format: absent fields → None → preserved.
        let declared = BlockCapabilities {
            crypto: true,
            collections: ["users"].iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let json = serde_json::json!({ "network_allow": ["https://a.com/"] });
        let overrides: ConfigCapabilityOverrides = serde_json::from_value(json).unwrap();
        let eff = declared.apply_config_overrides(&overrides);
        assert!(eff.crypto);
        assert!(eff.collections.contains("users"));
        assert_eq!(eff.network_allow, Vec::<String>::new()); // declared was empty; override narrowed to empty
    }
}
