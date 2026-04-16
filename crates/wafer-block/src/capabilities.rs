use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// BlockCapabilities declares what platform services a WASM block may access.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
