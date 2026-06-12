use std::{collections::HashMap, sync::Arc};

use parking_lot::RwLock;

use crate::interfaces::config::{handler, service::ConfigService};

crate::service_block! {
    /// Unified config block. Wraps any `ConfigService` implementation.
    block: pub ConfigBlock,
    name: "wafer-run/config",
    version: "0.0.1",
    interface: "config@v1",
    description: "Configuration key-value access",
    category: Service,
    fields: { service: Arc<dyn ConfigService> },
    handle: |this, _ctx, msg, body| handler::handle_message(this.service.as_ref(), &msg, &body),
}

/// Environment-variable backed [`ConfigService`] implementation.
///
/// Lookups check in-process runtime overrides first, then fall back to
/// `std::env::var`. Writes go to the override map, not the real environment.
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
