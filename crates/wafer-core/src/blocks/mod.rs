pub mod auth;
pub mod config;
pub mod cors;
#[cfg(feature = "crypto")]
pub mod crypto;
#[cfg(feature = "http")]
pub mod http;
pub mod iam;
pub mod inspector;
pub mod logger;
#[cfg(feature = "storage-local")]
pub mod local_storage;
pub mod monitoring;
#[cfg(feature = "network")]
pub mod network;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod rate_limit;
pub mod readonly_guard;
pub mod router;
#[cfg(feature = "storage-s3")]
pub mod s3_storage;
pub mod security_headers;
#[cfg(feature = "sqlite")]
pub mod sqlite;
pub mod web;

#[cfg(not(target_arch = "wasm32"))]
pub use config::ConfigBlock;
#[cfg(feature = "crypto")]
pub use crypto::CryptoBlock;
#[cfg(not(target_arch = "wasm32"))]
pub use logger::LoggerBlock;
#[cfg(feature = "network")]
pub use network::NetworkBlock;

// ---------------------------------------------------------------------------
// Helpers (used by block factories)
// ---------------------------------------------------------------------------

/// Read a config value, with env var override taking precedence.
pub fn env_or_config_str(
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
