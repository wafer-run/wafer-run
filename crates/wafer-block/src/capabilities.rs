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
    /// Unrestricted capabilities -- used by native Rust blocks.
    pub fn unrestricted() -> Self {
        Self {
            collections: {
                let mut s = HashSet::new();
                s.insert("*".to_string());
                s
            },
            raw_sql: true,
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

    /// No capabilities -- completely sandboxed.
    pub fn none() -> Self {
        Self {
            collections: HashSet::new(),
            raw_sql: false,
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

    pub fn allows_collection(&self, collection: &str) -> bool {
        self.collections.contains("*") || self.collections.contains(collection)
    }

    pub fn allows_storage_folder(&self, folder: &str) -> bool {
        self.storage_folders.contains("*") || self.storage_folders.contains(folder)
    }

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
}
