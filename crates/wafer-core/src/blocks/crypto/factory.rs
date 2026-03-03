//! Self-configuring BlockFactory for the crypto infrastructure block.

/// CryptoBlockFactory creates a CryptoBlock from config.
///
/// Config keys:
/// - `jwt_secret`: secret for JWT signing (auto-generated if empty)
///
/// Env var overrides: `JWT_SECRET`
#[cfg(feature = "crypto")]
pub struct CryptoBlockFactory;

#[cfg(feature = "crypto")]
impl wafer_run::registry::BlockFactory for CryptoBlockFactory {
    fn create(&self, config: Option<&serde_json::Value>) -> std::sync::Arc<dyn wafer_run::block::Block> {
        use std::sync::Arc;

        let jwt_secret = super::super::env_or_config_str("JWT_SECRET", config, "jwt_secret")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                tracing::warn!(
                    "JWT_SECRET not set — generating a random secret (tokens will not survive restarts)"
                );
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                use std::time::{SystemTime, UNIX_EPOCH};
                let mut hasher = DefaultHasher::new();
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
                    .hash(&mut hasher);
                std::process::id().hash(&mut hasher);
                let random_part = hasher.finish();
                format!("wafer-dev-secret-{:016x}", random_part)
            });

        let svc = super::service::Argon2JwtCryptoService::new(jwt_secret);
        Arc::new(super::block::CryptoBlock::new(Arc::new(svc)))
    }

    fn info(&self) -> wafer_run::block::BlockInfo {
        wafer_run::block::BlockInfo {
            name: "wafer/crypto".to_string(),
            version: "0.1.0".to_string(),
            interface: "wafer.infra.crypto".to_string(),
            summary: "Self-configuring crypto block factory".to_string(),
            instance_mode: wafer_run::types::InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }
}
