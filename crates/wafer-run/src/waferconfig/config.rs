use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// WaferConfig represents the top-level wafer.json configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaferConfig {
    #[serde(default)]
    pub services: ServiceConfig,
    #[serde(default)]
    pub blocks: Vec<String>,
    #[serde(default)]
    pub chains: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub config: HashMap<String, serde_json::Value>,
    #[serde(default = "default_port")]
    pub port: String,
}

fn default_port() -> String {
    "8090".to_string()
}

/// ServiceConfig declares the platform service providers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServiceConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<ProviderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<ProviderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logger: Option<ProviderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crypto: Option<ProviderConfig>,
}

/// ProviderConfig declares a specific service provider and its configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub provider: String,
    #[serde(default)]
    pub config: HashMap<String, serde_json::Value>,
}

/// Load reads and parses a wafer.json config file.
pub fn load_config(path: &str) -> Result<WaferConfig, String> {
    let data = std::fs::read_to_string(path)
        .map_err(|e| format!("read config {}: {}", path, e))?;
    parse_config(&data)
}

/// Parse parses a wafer.json config from raw JSON string.
pub fn parse_config(data: &str) -> Result<WaferConfig, String> {
    // Expand environment variables
    let expanded = expand_env_vars(data);

    let mut cfg: WaferConfig =
        serde_json::from_str(&expanded).map_err(|e| format!("parse config: {}", e))?;

    if cfg.port.is_empty() {
        cfg.port = "8090".to_string();
    }

    Ok(cfg)
}

/// Expand environment variables in the format $VAR or ${VAR}.
fn expand_env_vars(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' {
            if chars.peek() == Some(&'{') {
                chars.next(); // consume '{'
                let mut var_name = String::new();
                while let Some(&nc) = chars.peek() {
                    if nc == '}' {
                        chars.next();
                        break;
                    }
                    var_name.push(nc);
                    chars.next();
                }
                if let Ok(val) = std::env::var(&var_name) {
                    result.push_str(&val);
                }
            } else {
                let mut var_name = String::new();
                while let Some(&nc) = chars.peek() {
                    if nc.is_alphanumeric() || nc == '_' {
                        var_name.push(nc);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if !var_name.is_empty() {
                    if let Ok(val) = std::env::var(&var_name) {
                        result.push_str(&val);
                    }
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}
