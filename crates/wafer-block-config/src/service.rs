//! Environment-variable backed [`ConfigService`] implementation.
//!
//! Lookups check in-process runtime overrides first, then fall back to
//! `std::env::var`. Writes go to the override map, not the real environment.

use std::collections::HashMap;

use parking_lot::RwLock;
// Re-export the trait from wafer-core so consumers can use it.
pub use wafer_core::interfaces::config::service::ConfigService;

/// EnvConfigService reads config from environment variables with optional overrides.
pub struct EnvConfigService {
    overrides: RwLock<HashMap<String, String>>,
}

impl EnvConfigService {
    /// Create an empty service that reads from the process environment, with no overrides set.
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
