use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use crate::services::Services;
use super::config::ServiceConfig;

/// ProviderFactory creates a service from configuration.
pub type ProviderFactory = Arc<dyn Fn(&HashMap<String, serde_json::Value>) -> Result<Box<dyn std::any::Any + Send + Sync>, String> + Send + Sync>;

/// ProviderRegistry manages service provider factories.
pub struct ProviderRegistry {
    factories: RwLock<HashMap<String, ProviderFactory>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
        }
    }

    /// Register a provider factory under the given name.
    pub fn register(&self, name: impl Into<String>, factory: ProviderFactory) {
        self.factories.write().insert(name.into(), factory);
    }

    /// Get a provider factory by name.
    pub fn get(&self, name: &str) -> Option<ProviderFactory> {
        self.factories.read().get(name).cloned()
    }

    /// Create platform services from service configuration.
    pub fn create_services(&self, config: &ServiceConfig) -> Result<Services, String> {
        let mut services = Services::default();

        // Database
        if let Some(ref db_config) = config.database {
            if let Some(factory) = self.get(&db_config.provider) {
                let svc = factory(&db_config.config)?;
                if let Ok(db) = svc.downcast::<Arc<dyn crate::services::database::DatabaseService>>() {
                    services.database = Some(*db);
                }
            }
        }

        // Storage
        if let Some(ref storage_config) = config.storage {
            if let Some(factory) = self.get(&storage_config.provider) {
                let svc = factory(&storage_config.config)?;
                if let Ok(st) = svc.downcast::<Arc<dyn crate::services::storage::StorageService>>() {
                    services.storage = Some(*st);
                }
            }
        }

        // Logger
        if let Some(ref logger_config) = config.logger {
            if let Some(factory) = self.get(&logger_config.provider) {
                let svc = factory(&logger_config.config)?;
                if let Ok(lg) = svc.downcast::<Arc<dyn crate::services::logger::LoggerService>>() {
                    services.logger = Some(*lg);
                }
            }
        }

        // Crypto
        if let Some(ref crypto_config) = config.crypto {
            if let Some(factory) = self.get(&crypto_config.provider) {
                let svc = factory(&crypto_config.config)?;
                if let Ok(cr) = svc.downcast::<Arc<dyn crate::services::crypto::CryptoService>>() {
                    services.crypto = Some(*cr);
                }
            }
        }

        Ok(services)
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}
