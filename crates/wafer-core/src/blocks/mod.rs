// Infrastructure blocks (moved from wafer-run)
pub mod config;
pub mod crypto;
pub mod database;
pub mod logger;
pub mod network;
pub mod storage;

pub use config::ConfigBlock;
pub use crypto::CryptoBlock;
pub use database::DatabaseBlock;
pub use logger::LoggerBlock;
pub use network::NetworkBlock;
pub use storage::StorageBlock;

// Application blocks
pub mod auth;
pub mod cors;
pub mod http_router;
pub mod iam;
pub mod monitoring;
pub mod rate_limit;
pub mod readonly_guard;
pub mod security_headers;
pub mod web;

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
