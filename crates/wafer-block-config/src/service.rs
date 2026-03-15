use parking_lot::RwLock;
use std::collections::HashMap;

/// Service provides key-value configuration access.
pub trait ConfigService: wafer_run::MaybeSend + wafer_run::MaybeSync {
    /// Get retrieves a config value by key.
    fn get(&self, key: &str) -> Option<String>;

    /// GetDefault retrieves a config value, returning default_value if not found.
    fn get_default(&self, key: &str, default_value: &str) -> String {
        self.get(key).unwrap_or_else(|| default_value.to_string())
    }

    /// Set stores a config key-value pair.
    fn set(&self, key: &str, value: &str);
}

/// EnvConfigService reads config from environment variables.
pub struct EnvConfigService {
    overrides: RwLock<HashMap<String, String>>,
}

impl EnvConfigService {
    pub fn new() -> Self {
        Self {
            overrides: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for EnvConfigService {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigService for EnvConfigService {
    fn get(&self, key: &str) -> Option<String> {
        // Check overrides first
        if let Some(val) = self.overrides.read().get(key) {
            return Some(val.clone());
        }
        // Then environment
        std::env::var(key).ok()
    }

    fn set(&self, key: &str, value: &str) {
        self.overrides
            .write()
            .insert(key.to_string(), value.to_string());
    }
}
