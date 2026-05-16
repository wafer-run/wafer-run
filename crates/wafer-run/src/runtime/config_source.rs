//! Per-block lazy config loading.
//!
//! Implementations live with their consumers (D1 in solobase-cloudflare,
//! env in solobase-core, static here for tests).
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §2

use std::collections::HashMap;

use async_trait::async_trait;
use thiserror::Error;
use wafer_block::ConfigVar;

/// The env-var config payload returned for a single block on lazy init.
///
/// Wraps a `HashMap<String, String>` of SCREAMING_SNAKE env-var keys so
/// callers can't accidentally mix in unrelated keys from other blocks.
///
/// Named `EnvBlockConfig` to distinguish from the flow-event JSON config type
/// (`wafer_block::config::BlockConfig`) that blocks read via
/// `BlockConfig::from_event`.
#[derive(Debug, Clone, Default)]
pub struct EnvBlockConfig {
    inner: HashMap<String, String>,
}

impl EnvBlockConfig {
    /// Construct from a `HashMap`. Intended for `ConfigSource` implementors;
    /// block code reads values via [`EnvBlockConfig::get`] after the runtime
    /// hands them an `EnvBlockConfig` from `load_for_block`.
    pub fn new(inner: HashMap<String, String>) -> Self {
        Self { inner }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.inner.get(key).map(String::as_str)
    }

    pub fn into_inner(self) -> HashMap<String, String> {
        self.inner
    }
}

/// Errors returned by [`ConfigSource::load_for_block`].
#[derive(Debug, Error)]
pub enum ConfigError {
    /// A `required: true` key (i.e. `optional: false`) has no value in the
    /// source and no non-empty default in its `ConfigVar`.
    #[error("required config key `{key}` missing for block `{block}`")]
    MissingRequired { block: String, key: String },

    /// A transient I/O error (network timeout, D1 failure). The caller may
    /// retry; the error is not cached in the block slot.
    #[error("transient error fetching config for `{block}`: {source}")]
    Transient {
        block: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Source of per-block env-var configuration, consulted on first block init.
///
/// Implementations:
/// - `StaticConfigSource` — in-memory `HashMap` for tests (this module).
/// - `EnvConfigSource` — reads `std::env::var` (solobase-core, PR 2).
/// - `D1ConfigSource` — reads Cloudflare D1 (solobase-cloudflare, PR 2).
#[async_trait]
pub trait ConfigSource: Send + Sync + 'static {
    /// Load the values for `block`'s declared env-var config keys.
    ///
    /// Implementations should:
    /// - Return values for every declared key present in the source.
    /// - Fall back to `ConfigVar::default` for keys not present in the source
    ///   when the default is non-empty.
    /// - Return [`ConfigError::MissingRequired`] for keys where `optional ==
    ///   false`, no value is present in the source, and the default is empty.
    /// - Return [`ConfigError::Transient`] for I/O failures (network, D1 timeout).
    /// - Ignore source keys that are not in `declared_keys`.
    async fn load_for_block(
        &self,
        block: &str,
        declared_keys: &[ConfigVar],
    ) -> Result<EnvBlockConfig, ConfigError>;
}

/// In-memory [`ConfigSource`]. Used by tests and as a stand-in until the real
/// D1 / env implementations land in PR 2.
#[derive(Debug, Clone, Default)]
pub struct StaticConfigSource {
    data: HashMap<String, String>,
}

impl StaticConfigSource {
    pub fn new(data: HashMap<String, String>) -> Self {
        Self { data }
    }
}

#[async_trait]
impl ConfigSource for StaticConfigSource {
    async fn load_for_block(
        &self,
        block: &str,
        declared_keys: &[ConfigVar],
    ) -> Result<EnvBlockConfig, ConfigError> {
        let mut out = HashMap::with_capacity(declared_keys.len());
        for var in declared_keys {
            if let Some(v) = self.data.get(&var.key) {
                out.insert(var.key.clone(), v.clone());
            } else if !var.default.is_empty() {
                out.insert(var.key.clone(), var.default.clone());
            } else if !var.optional {
                // optional == false means "required"
                return Err(ConfigError::MissingRequired {
                    block: block.to_string(),
                    key: var.key.clone(),
                });
            }
            // optional == true + no value + no default: skip; caller's get() returns None
        }
        Ok(EnvBlockConfig::new(out))
    }
}
