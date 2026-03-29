use parking_lot::RwLock;
use std::collections::HashMap;

// Re-export the trait from wafer-core so consumers can use it.
pub use wafer_core::interfaces::config::service::ConfigService;

/// EnvConfigService reads config from environment variables with optional overrides.
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
