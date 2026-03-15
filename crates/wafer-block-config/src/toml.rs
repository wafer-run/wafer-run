//! TOML + environment variable layered config service.
//!
//! Priority: runtime overrides > env vars > TOML file values > defaults.

use super::service::ConfigService;
use parking_lot::RwLock;
use std::collections::HashMap;

/// Maps environment variable names to their TOML equivalents.
const ENV_ALIASES: &[(&str, &str)] = &[
    ("DB_TYPE", "database.type"),
    ("DB_PATH", "database.path"),
    ("DATABASE_URL", "database.url"),
    ("STORAGE_TYPE", "storage.type"),
    ("STORAGE_ROOT", "storage.root"),
    ("S3_BUCKET", "storage.bucket"),
    ("S3_REGION", "storage.region"),
    ("S3_ENDPOINT", "storage.endpoint"),
    ("S3_PREFIX", "storage.prefix"),
    ("JWT_SECRET", "auth.jwt_secret"),
    ("BIND_ADDR", "server.bind"),
    ("LOG_FORMAT", "server.log_format"),
    ("ENABLE_SIGNUP", "auth.enable_signup"),
];

/// TomlConfigService reads config from a TOML file with env var overrides.
///
/// Keys can be looked up by either their env var name (e.g. `DB_TYPE`)
/// or their TOML dotted path (e.g. `database.type`). Both resolve to the
/// same underlying value.
pub struct TomlConfigService {
    file_values: HashMap<String, String>,
    overrides: RwLock<HashMap<String, String>>,
}

impl TomlConfigService {
    /// Load config from a TOML file.
    pub fn load(path: &str) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("reading config file {}: {}", path, e))?;
        let table: toml::Table = content
            .parse()
            .map_err(|e| format!("parsing TOML {}: {}", path, e))?;
        let file_values = flatten_toml(&table, "");
        Ok(Self {
            file_values,
            overrides: RwLock::new(HashMap::new()),
        })
    }

    /// Try loading from path; return empty config if file doesn't exist or fails.
    pub fn load_or_default(path: &str) -> Self {
        match Self::load(path) {
            Ok(s) => {
                tracing::info!(path = %path, keys = s.file_values.len(), "loaded config from TOML");
                s
            }
            Err(e) => {
                tracing::debug!(path = %path, error = %e, "no TOML config loaded, using env vars only");
                Self {
                    file_values: HashMap::new(),
                    overrides: RwLock::new(HashMap::new()),
                }
            }
        }
    }

    /// Look up the TOML dotted key for an env-style key, or return the key as-is.
    fn resolve_key(key: &str) -> &str {
        for &(env_name, toml_name) in ENV_ALIASES {
            if key == env_name {
                return toml_name;
            }
        }
        key
    }

    /// Look up the env var name for a TOML dotted key, if one exists.
    fn resolve_env_key(key: &str) -> Option<&'static str> {
        for &(env_name, toml_name) in ENV_ALIASES {
            if key == toml_name {
                return Some(env_name);
            }
        }
        None
    }
}

impl ConfigService for TomlConfigService {
    fn get(&self, key: &str) -> Option<String> {
        // 1. Runtime overrides (check both key forms)
        {
            let overrides = self.overrides.read();
            if let Some(val) = overrides.get(key) {
                return Some(val.clone());
            }
            let toml_key = Self::resolve_key(key);
            if toml_key != key {
                if let Some(val) = overrides.get(toml_key) {
                    return Some(val.clone());
                }
            }
        }

        // 2. Environment variable — try the key directly
        if let Ok(val) = std::env::var(key) {
            return Some(val);
        }
        // Also try the env alias for a TOML key
        let toml_key = Self::resolve_key(key);
        if let Some(env_name) = Self::resolve_env_key(toml_key) {
            if env_name != key {
                if let Ok(val) = std::env::var(env_name) {
                    return Some(val);
                }
            }
        }

        // 3. TOML file values (try resolved toml key)
        if let Some(val) = self.file_values.get(toml_key) {
            return Some(val.clone());
        }
        // Also try the original key directly in file values
        if toml_key != key {
            if let Some(val) = self.file_values.get(key) {
                return Some(val.clone());
            }
        }

        None
    }

    fn set(&self, key: &str, value: &str) {
        self.overrides
            .write()
            .insert(key.to_string(), value.to_string());
    }
}

/// Flatten a TOML table into dot-separated key-value pairs.
///
/// `[database] type = "sqlite"` becomes `"database.type" => "sqlite"`.
fn flatten_toml(table: &toml::Table, prefix: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (key, value) in table {
        let full_key = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{}.{}", prefix, key)
        };
        match value {
            toml::Value::Table(sub) => {
                out.extend(flatten_toml(sub, &full_key));
            }
            toml::Value::String(s) => {
                out.insert(full_key, s.clone());
            }
            toml::Value::Integer(n) => {
                out.insert(full_key, n.to_string());
            }
            toml::Value::Float(f) => {
                out.insert(full_key, f.to_string());
            }
            toml::Value::Boolean(b) => {
                out.insert(full_key, b.to_string());
            }
            toml::Value::Array(arr) => {
                // Store arrays as JSON for complex values
                if let Ok(json) = serde_json::to_string(arr) {
                    out.insert(full_key, json);
                }
            }
            toml::Value::Datetime(dt) => {
                out.insert(full_key, dt.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flatten_toml() {
        let toml_str = r#"
[server]
bind = "0.0.0.0:8090"

[database]
type = "sqlite"
path = "data/test.db"

[storage]
type = "local"
root = "data/storage"

[auth]
jwt_secret = "test-secret"
enable_signup = true

[features]
auth = true
admin = true
products = false
"#;
        let table: toml::Table = toml_str.parse().unwrap();
        let flat = flatten_toml(&table, "");

        assert_eq!(flat.get("server.bind").unwrap(), "0.0.0.0:8090");
        assert_eq!(flat.get("database.type").unwrap(), "sqlite");
        assert_eq!(flat.get("database.path").unwrap(), "data/test.db");
        assert_eq!(flat.get("storage.type").unwrap(), "local");
        assert_eq!(flat.get("storage.root").unwrap(), "data/storage");
        assert_eq!(flat.get("auth.jwt_secret").unwrap(), "test-secret");
        assert_eq!(flat.get("auth.enable_signup").unwrap(), "true");
        assert_eq!(flat.get("features.auth").unwrap(), "true");
        assert_eq!(flat.get("features.products").unwrap(), "false");
    }

    #[test]
    fn test_toml_config_service_get_by_toml_key() {
        let toml_str = r#"
[database]
type = "postgres"
url = "postgres://localhost/test"
"#;
        let table: toml::Table = toml_str.parse().unwrap();
        let svc = TomlConfigService {
            file_values: flatten_toml(&table, ""),
            overrides: RwLock::new(HashMap::new()),
        };

        // Direct TOML key lookup
        assert_eq!(svc.get("database.type").unwrap(), "postgres");
        assert_eq!(svc.get("database.url").unwrap(), "postgres://localhost/test");
    }

    #[test]
    fn test_toml_config_service_get_by_env_alias() {
        let toml_str = r#"
[database]
type = "sqlite"
path = "data/my.db"

[storage]
type = "s3"
bucket = "my-bucket"
"#;
        let table: toml::Table = toml_str.parse().unwrap();
        let svc = TomlConfigService {
            file_values: flatten_toml(&table, ""),
            overrides: RwLock::new(HashMap::new()),
        };

        // Env alias lookup resolves to TOML key
        assert_eq!(svc.get("DB_TYPE").unwrap(), "sqlite");
        assert_eq!(svc.get("DB_PATH").unwrap(), "data/my.db");
        assert_eq!(svc.get("STORAGE_TYPE").unwrap(), "s3");
        assert_eq!(svc.get("S3_BUCKET").unwrap(), "my-bucket");
    }

    #[test]
    fn test_overrides_take_priority() {
        let toml_str = r#"
[database]
type = "sqlite"
"#;
        let table: toml::Table = toml_str.parse().unwrap();
        let svc = TomlConfigService {
            file_values: flatten_toml(&table, ""),
            overrides: RwLock::new(HashMap::new()),
        };

        assert_eq!(svc.get("database.type").unwrap(), "sqlite");

        svc.set("database.type", "postgres");
        assert_eq!(svc.get("database.type").unwrap(), "postgres");
        // Env alias also sees the override
        assert_eq!(svc.get("DB_TYPE").unwrap(), "postgres");
    }

    #[test]
    fn test_missing_key_returns_none() {
        let svc = TomlConfigService {
            file_values: HashMap::new(),
            overrides: RwLock::new(HashMap::new()),
        };
        assert!(svc.get("nonexistent.key").is_none());
        assert!(svc.get("DB_TYPE").is_none());
    }

    #[test]
    fn test_get_default() {
        let svc = TomlConfigService {
            file_values: HashMap::new(),
            overrides: RwLock::new(HashMap::new()),
        };
        assert_eq!(svc.get_default("DB_TYPE", "sqlite"), "sqlite");
        assert_eq!(
            svc.get_default("nonexistent", "fallback"),
            "fallback"
        );
    }

    #[test]
    fn test_load_or_default_with_missing_file() {
        let svc = TomlConfigService::load_or_default("/nonexistent/path/config.toml");
        assert!(svc.file_values.is_empty());
        assert!(svc.get("database.type").is_none());
    }

    #[test]
    fn test_boolean_and_numeric_values() {
        let toml_str = r#"
[features]
auth = true
products = false

[server]
port = 8090
"#;
        let table: toml::Table = toml_str.parse().unwrap();
        let flat = flatten_toml(&table, "");

        assert_eq!(flat.get("features.auth").unwrap(), "true");
        assert_eq!(flat.get("features.products").unwrap(), "false");
        assert_eq!(flat.get("server.port").unwrap(), "8090");
    }
}
