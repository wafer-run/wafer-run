use serde::Deserialize;
use std::collections::HashSet;

/// BlockCapabilities declares what platform services a WASM block may access.
#[derive(Debug, Clone, Deserialize)]
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
            return true; // any URL if network=true and no allowlist
        }
        self.network_allow
            .iter()
            .any(|prefix| url.starts_with(prefix))
    }

    pub fn allows_config_key(&self, key: &str) -> bool {
        if self.config_keys.is_empty() {
            return true; // any key if config=true and no key restrictions
        }
        self.config_keys.contains(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allows_collection_wildcard() {
        let caps = BlockCapabilities::unrestricted();
        assert!(caps.allows_collection("users"));
        assert!(caps.allows_collection("posts"));
        assert!(caps.allows_collection("anything"));
    }

    #[test]
    fn test_allows_collection_specific() {
        let mut caps = BlockCapabilities::none();
        caps.collections.insert("users".to_string());
        caps.collections.insert("posts".to_string());
        assert!(caps.allows_collection("users"));
        assert!(caps.allows_collection("posts"));
        assert!(!caps.allows_collection("secrets"));
    }

    #[test]
    fn test_allows_collection_empty() {
        let caps = BlockCapabilities::none();
        assert!(!caps.allows_collection("users"));
    }

    #[test]
    fn test_allows_storage_folder_wildcard() {
        let caps = BlockCapabilities::unrestricted();
        assert!(caps.allows_storage_folder("uploads"));
        assert!(caps.allows_storage_folder("backups"));
    }

    #[test]
    fn test_allows_storage_folder_specific() {
        let mut caps = BlockCapabilities::none();
        caps.storage_folders.insert("uploads".to_string());
        assert!(caps.allows_storage_folder("uploads"));
        assert!(!caps.allows_storage_folder("backups"));
    }

    #[test]
    fn test_allows_storage_folder_empty() {
        let caps = BlockCapabilities::none();
        assert!(!caps.allows_storage_folder("uploads"));
    }

    #[test]
    fn test_allows_network_url_disabled() {
        let caps = BlockCapabilities::none();
        assert!(!caps.allows_network_url("https://example.com"));
    }

    #[test]
    fn test_allows_network_url_enabled_no_allowlist() {
        let mut caps = BlockCapabilities::none();
        caps.network = true;
        assert!(caps.allows_network_url("https://example.com"));
        assert!(caps.allows_network_url("https://evil.com"));
    }

    #[test]
    fn test_allows_network_url_with_allowlist() {
        let mut caps = BlockCapabilities::none();
        caps.network = true;
        caps.network_allow = vec![
            "https://api.example.com/".to_string(),
            "https://cdn.example.com/".to_string(),
        ];
        assert!(caps.allows_network_url("https://api.example.com/v1/users"));
        assert!(caps.allows_network_url("https://cdn.example.com/images/logo.png"));
        assert!(!caps.allows_network_url("https://evil.com/steal"));
        assert!(!caps.allows_network_url("https://api.example.org/v1"));
    }

    #[test]
    fn test_allows_config_key_no_restrictions() {
        let caps = BlockCapabilities::unrestricted();
        assert!(caps.allows_config_key("any_key"));
        assert!(caps.allows_config_key("secret"));
    }

    #[test]
    fn test_allows_config_key_with_restrictions() {
        let mut caps = BlockCapabilities::none();
        caps.config = true;
        caps.config_keys.insert("allowed_key".to_string());
        caps.config_keys.insert("another_key".to_string());
        assert!(caps.allows_config_key("allowed_key"));
        assert!(caps.allows_config_key("another_key"));
        assert!(!caps.allows_config_key("secret"));
    }

    #[test]
    fn test_unrestricted() {
        let caps = BlockCapabilities::unrestricted();
        assert!(caps.collections.contains("*"));
        assert!(caps.raw_sql);
        assert!(caps.storage_folders.contains("*"));
        assert!(caps.crypto);
        assert!(caps.network);
        assert!(caps.network_allow.is_empty());
        assert!(caps.config);
        assert!(caps.config_keys.is_empty());
    }

    #[test]
    fn test_none() {
        let caps = BlockCapabilities::none();
        assert!(caps.collections.is_empty());
        assert!(!caps.raw_sql);
        assert!(caps.storage_folders.is_empty());
        assert!(!caps.crypto);
        assert!(!caps.network);
        assert!(caps.network_allow.is_empty());
        assert!(!caps.config);
        assert!(caps.config_keys.is_empty());
    }

    #[test]
    fn test_deserialize_from_json() {
        let json = r#"{
            "collections": ["users", "posts"],
            "raw_sql": true,
            "storage_folders": ["uploads"],
            "crypto": true,
            "network": true,
            "network_allow": ["https://api.example.com/"],
            "config": true,
            "config_keys": ["db_url"]
        }"#;
        let caps: BlockCapabilities = serde_json::from_str(json).unwrap();
        assert!(caps.collections.contains("users"));
        assert!(caps.collections.contains("posts"));
        assert_eq!(caps.collections.len(), 2);
        assert!(caps.raw_sql);
        assert!(caps.storage_folders.contains("uploads"));
        assert!(caps.crypto);
        assert!(caps.network);
        assert_eq!(caps.network_allow, vec!["https://api.example.com/"]);
        assert!(caps.config);
        assert!(caps.config_keys.contains("db_url"));
    }

    #[test]
    fn test_deserialize_defaults() {
        let json = r#"{}"#;
        let caps: BlockCapabilities = serde_json::from_str(json).unwrap();
        assert!(caps.collections.is_empty());
        assert!(!caps.raw_sql);
        assert!(caps.storage_folders.is_empty());
        assert!(!caps.crypto);
        assert!(!caps.network);
        assert!(caps.network_allow.is_empty());
        assert!(!caps.config);
        assert!(caps.config_keys.is_empty());
    }

    #[test]
    fn test_deserialize_partial() {
        let json = r#"{"collections": ["*"], "network": true}"#;
        let caps: BlockCapabilities = serde_json::from_str(json).unwrap();
        assert!(caps.allows_collection("anything"));
        assert!(caps.network);
        assert!(!caps.crypto);
        assert!(!caps.config);
    }
}
