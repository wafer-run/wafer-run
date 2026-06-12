//! Per-block lazy config loading.
//!
//! Implementations live with their consumers (D1 in solobase-cloudflare,
//! env in solobase-core, static here for tests).
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §2

use std::{collections::HashMap, sync::Arc};

use thiserror::Error;
use wafer_block::ConfigVar;
use wafer_block_macro::wafer_async_trait;

use crate::compat::{MaybeSend, MaybeSync};

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

    /// Look up a value by its SCREAMING_SNAKE env-var key.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.inner.get(key).map(String::as_str)
    }

    /// Consume `self` and yield the underlying key→value map.
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
    MissingRequired {
        /// Block name being initialised.
        block: String,
        /// SCREAMING_SNAKE key that was required but not supplied.
        key: String,
    },

    /// A transient I/O error (network timeout, D1 failure). The caller may
    /// retry; the error is not cached in the block slot.
    #[error("transient error fetching config for `{block}`: {source}")]
    Transient {
        /// Block name whose config load failed.
        block: String,
        /// Underlying transport error.
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
#[wafer_async_trait]
pub trait ConfigSource: MaybeSend + MaybeSync + 'static {
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
    /// Build a source backed by an explicit key→value map.
    pub fn new(data: HashMap<String, String>) -> Self {
        Self { data }
    }
}

#[wafer_async_trait]
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

/// Walk the given blocks' [`BlockInfo::config_keys`](wafer_block::BlockInfo)
/// declarations and ask `source` to load values, reporting which blocks have
/// missing required keys or unreachable sources.
///
/// Shared by `Wafer::validate_all_block_configs` and the
/// [`Context::validate_all_block_configs`](wafer_block::context::Context)
/// impl on [`RuntimeContext`](crate::context::RuntimeContext) so the two
/// surfaces cannot drift. Callers must pass the **canonical** block view
/// (registered names only, no aliases) so aliased blocks aren't validated
/// and reported twice.
///
/// Does **not** invoke any block's `lifecycle` or `handle`.
pub(crate) async fn validate_block_configs<'a>(
    blocks: impl IntoIterator<Item = (&'a String, &'a Arc<dyn crate::block::Block>)>,
    source: &Arc<dyn ConfigSource>,
) -> wafer_block::ValidationReport {
    let mut report = wafer_block::ValidationReport {
        ok: Vec::new(),
        broken: Vec::new(),
    };
    for (name, block) in blocks {
        let info = block.info();
        match source.load_for_block(name, &info.config_keys).await {
            Ok(_) => report.ok.push(name.clone()),
            Err(ConfigError::MissingRequired { block, key }) => {
                report.broken.push(wafer_block::BrokenBlock {
                    block,
                    missing_keys: vec![key],
                });
            }
            Err(ConfigError::Transient { block, .. }) => {
                report.broken.push(wafer_block::BrokenBlock {
                    block,
                    missing_keys: vec!["<transient: source unreachable>".to_string()],
                });
            }
        }
    }
    report.ok.sort();
    report.broken.sort_by(|a, b| a.block.cmp(&b.block));
    report
}

/// The runtime's config state: the lazy per-block [`ConfigSource`] consulted
/// on first init, plus the embedder-supplied synchronous config snapshot
/// layered under per-call config. Bundled together because both are the
/// runtime's configuration inputs and are cloned as a pair into every
/// [`RuntimeContext`](crate::context::RuntimeContext).
pub(crate) struct ConfigState {
    /// Source of per-block env-var config, consulted on first init (async,
    /// serves the `wafer-run/config` block's `config::get(ctx, key).await`).
    pub(crate) source: Arc<dyn ConfigSource>,
    /// Embedder-supplied env-style snapshot, cloned into every
    /// [`RuntimeContext`] so `ctx.config_get(key)` resolves boot-time values
    /// regardless of lifecycle stage. Empty until [`set_snapshot`](Self::set_snapshot).
    pub(crate) snapshot: Arc<HashMap<String, String>>,
}

impl ConfigState {
    /// Construct with the given source and an empty snapshot.
    pub(crate) fn new(source: Arc<dyn ConfigSource>) -> Self {
        Self {
            source,
            snapshot: Arc::new(HashMap::new()),
        }
    }

    /// Construct with the default in-memory [`StaticConfigSource`] (used by
    /// `Wafer::empty()` before the builder installs the real source).
    pub(crate) fn default_static() -> Self {
        Self::new(Arc::new(StaticConfigSource::default()))
    }

    /// Replace the snapshot. Backs `Wafer::set_config_snapshot`.
    pub(crate) fn set_snapshot(&mut self, snapshot: HashMap<String, String>) {
        self.snapshot = Arc::new(snapshot);
    }

    /// Borrow the snapshot Arc. Backs `Wafer::config_snapshot`.
    pub(crate) fn snapshot(&self) -> &Arc<HashMap<String, String>> {
        &self.snapshot
    }
}
