pub mod auth;
pub mod config;
pub mod cors;
pub mod crypto;
pub mod database;
#[cfg(feature = "http")]
pub mod http;
pub mod iam;
pub mod inspector;
pub mod logger;
pub mod monitoring;
pub mod network;
pub mod rate_limit;
pub mod readonly_guard;
pub mod router;
pub mod security_headers;
pub mod storage;
pub mod web;

pub use config::ConfigBlock;
pub use crypto::CryptoBlock;
pub use database::DatabaseBlock;
pub use logger::LoggerBlock;
pub use network::NetworkBlock;
pub use storage::StorageBlock;

// ---------------------------------------------------------------------------
// Helpers (used by block factories)
// ---------------------------------------------------------------------------

/// Read a config value, with env var override taking precedence.
pub(crate) fn env_or_config_str(
    env_var: &str,
    config: Option<&serde_json::Value>,
    key: &str,
) -> Option<String> {
    // Env var takes precedence
    if let Ok(val) = std::env::var(env_var) {
        if !val.is_empty() {
            return Some(val);
        }
    }
    // Then JSON config
    config
        .and_then(|c| c.get(key))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}
