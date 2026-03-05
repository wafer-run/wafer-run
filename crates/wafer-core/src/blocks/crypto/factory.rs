//! BlockFactory for the crypto block.

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
                let mode = std::env::var("WAFER_ENV").unwrap_or_default();
                if mode == "production" || mode == "prod" {
                    tracing::error!(
                        "JWT_SECRET not set in production — set JWT_SECRET environment variable. \
                         Using auto-generated secret; tokens will NOT survive restarts."
                    );
                } else {
                    tracing::warn!(
                        "JWT_SECRET not set — generating a random secret (tokens will not survive restarts)"
                    );
                }
                // Use two UUIDs v4 (backed by OS CSPRNG) for 256 bits of entropy
                format!(
                    "wafer-auto-{}{}",
                    uuid::Uuid::new_v4().as_simple(),
                    uuid::Uuid::new_v4().as_simple(),
                )
            });

        let svc = super::service::Argon2JwtCryptoService::new(jwt_secret);
        Arc::new(super::block::CryptoBlock::new(Arc::new(svc)))
    }

    fn info(&self) -> wafer_run::block::BlockInfo {
        wafer_run::block::BlockInfo {
            name: "@wafer/crypto".to_string(),
            version: "0.1.0".to_string(),
            interface: "crypto@v1".to_string(),
            summary: "Crypto block factory".to_string(),
            instance_mode: wafer_run::types::InstanceMode::PerNode,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: wafer_run::types::BlockRuntime::Native,
            requires: Vec::new(),
        }
    }
}
