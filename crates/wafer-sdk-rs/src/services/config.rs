//! Config service client using WIT-generated imports.

use crate::wafer::block_world::config as wit;

/// Retrieve a configuration value by key, returning `None` if not found.
pub fn get(key: &str) -> Option<String> {
    wit::get(key)
}

/// Retrieve a config value, returning `default_value` if the key is absent.
pub fn get_default(key: &str, default_value: &str) -> String {
    wit::get(key).unwrap_or_else(|| default_value.to_string())
}

/// Store a configuration key-value pair.
pub fn set(key: &str, value: &str) {
    wit::set(key, value);
}
