//! Per-block lazy config loading.
//!
//! Implementations live with their consumers (D1 in the Cloudflare Workers app,
//! env in the native app, static here for tests).
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §2

use std::{collections::HashMap, sync::Arc};

use thiserror::Error;
use wafer_block::{
    compat::{MaybeSend, MaybeSync},
    ConfigVar,
};
use wafer_block_macro::wafer_async_trait;

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
/// - `EnvConfigSource` — reads `std::env::var` (the native app, PR 2).
/// - `D1ConfigSource` — reads Cloudflare D1 (the Cloudflare Workers app, PR 2).
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
        resolve_declared(block, declared_keys, |key| self.data.get(key).cloned())
    }
}

/// Resolve a block's declared [`ConfigVar`]s against an arbitrary source.
///
/// This is the body every [`ConfigSource`] implementation shares once its own
/// storage is factored out: walk the block's declarations, take what the
/// source has, fall back to the declared default, and decide whether what is
/// left is an error or an omission. Implementations supply only `lookup`,
/// which answers "what does my storage hold for this key" — an env read, a
/// D1 row, a `HashMap` get, an overlay checked before a table.
///
/// It exists so that decision cannot drift between sources. The rules, in
/// order, per declared key:
///
/// 1. `lookup(key)` returns `Some(v)` → the resolved value is `v`.
/// 2. Otherwise, if the declaration carries a **non-empty**
///    [`ConfigVar::default`] → the default.
/// 3. Otherwise, if the declaration is **required** (`optional == false`) →
///    [`ConfigError::MissingRequired`], naming `block` and the key. The first
///    such key stops resolution; the error names one key, not all of them.
/// 4. Otherwise the key is **omitted** from the returned map, so the block's
///    `get()` answers `None`. It is not inserted as an empty string — a block
///    cannot tell a blank setting from an unset one.
///
/// Keys the source holds but the block does not declare are never consulted
/// and never appear in the result.
///
/// # `lookup` owns the meaning of an empty value
///
/// The helper decides presence, not interpretation: `Some(String::new())` is
/// a value, and it satisfies a required key and beats the default. A source
/// for which an empty stored value means "unset" must express that itself —
/// `….filter(|s| !s.is_empty())` — rather than expecting this function to
/// guess. In-tree sources genuinely differ here (an env var set to the empty
/// string is set; an empty column in a settings table usually is not), which
/// is why the policy stays with the caller that knows its storage.
pub fn resolve_declared(
    block: &str,
    declared_keys: &[ConfigVar],
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<EnvBlockConfig, ConfigError> {
    let mut out = HashMap::with_capacity(declared_keys.len());
    for var in declared_keys {
        let resolved = lookup(&var.key).or_else(|| {
            if var.default.is_empty() {
                None
            } else {
                Some(var.default.clone())
            }
        });
        match resolved {
            Some(v) => {
                out.insert(var.key.clone(), v);
            }
            // `optional == false` means "required".
            None if !var.optional => {
                return Err(ConfigError::MissingRequired {
                    block: block.to_string(),
                    key: var.key.clone(),
                });
            }
            None => {
                // Optional, no value, no default: omitted on purpose.
            }
        }
    }
    Ok(EnvBlockConfig::new(out))
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
    blocks: impl IntoIterator<Item = (&'a String, &'a Arc<dyn wafer_block::Block>)>,
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

#[cfg(test)]
mod resolve_declared_tests {
    use super::*;

    fn required(key: &str, default: &str) -> ConfigVar {
        ConfigVar::new(key, "", default)
    }

    fn optional(key: &str, default: &str) -> ConfigVar {
        let mut v = ConfigVar::new(key, "", default);
        v.optional = true;
        v
    }

    fn nothing(_key: &str) -> Option<String> {
        None
    }

    #[test]
    fn a_present_value_wins_over_the_default() {
        let declared = [required("A", "from-default")];
        let out = resolve_declared("blk", &declared, |k| {
            (k == "A").then(|| "from-source".to_string())
        })
        .expect("resolves");
        assert_eq!(out.get("A"), Some("from-source"));
    }

    #[test]
    fn an_absent_value_falls_back_to_a_non_empty_default() {
        let declared = [required("A", "fallback")];
        let out = resolve_declared("blk", &declared, nothing).expect("resolves");
        assert_eq!(out.get("A"), Some("fallback"));
    }

    #[test]
    fn a_required_key_with_no_value_and_no_default_is_an_error() {
        let declared = [required("A", "")];
        let err = resolve_declared("blk", &declared, nothing).expect_err("must fail");
        match err {
            ConfigError::MissingRequired { block, key } => {
                assert_eq!(block, "blk");
                assert_eq!(key, "A");
            }
            other => panic!("expected MissingRequired, got {other:?}"),
        }
    }

    /// An optional key with nothing behind it is simply absent from the map;
    /// the block's `get()` returns `None` and the block degrades. It is not
    /// an empty string, which a block could not distinguish from a
    /// deliberately-blank setting.
    #[test]
    fn an_optional_key_with_no_value_is_omitted_not_blank() {
        let declared = [optional("A", "")];
        let out = resolve_declared("blk", &declared, nothing).expect("resolves");
        assert_eq!(out.get("A"), None);
        assert!(out.into_inner().is_empty());
    }

    #[test]
    fn only_declared_keys_are_resolved() {
        let declared = [required("A", "a")];
        let out = resolve_declared("blk", &declared, |k| Some(format!("v-{k}"))).expect("resolves");
        let map = out.into_inner();
        assert_eq!(
            map.len(),
            1,
            "undeclared source keys must not appear: {map:?}"
        );
        assert_eq!(map.get("A").map(String::as_str), Some("v-A"));
    }

    /// The helper does not interpret values, only presence. A source where
    /// an empty string means "unset" must say so by returning `None` from
    /// its lookup — otherwise the empty string is a real value and satisfies
    /// a required key. This is the one policy decision the helper delegates,
    /// and the three in-tree sources genuinely disagree about it, so it has
    /// to be the caller's.
    #[test]
    fn an_empty_string_from_the_lookup_is_a_value_not_an_absence() {
        let declared = [required("A", "fallback")];
        let out = resolve_declared("blk", &declared, |_| Some(String::new())).expect("resolves");
        assert_eq!(out.get("A"), Some(""));

        // …and a lookup that filters empties itself gets the default.
        let out = resolve_declared("blk", &declared, |_| {
            Some(String::new()).filter(|s| !s.is_empty())
        })
        .expect("resolves");
        assert_eq!(out.get("A"), Some("fallback"));
    }

    /// The first missing required key is reported; resolution stops there
    /// rather than accumulating.
    #[test]
    fn the_first_missing_required_key_is_the_one_reported() {
        let declared = [required("A", ""), required("B", "")];
        let err = resolve_declared("blk", &declared, nothing).expect_err("must fail");
        assert!(
            matches!(&err, ConfigError::MissingRequired { key, .. } if key == "A"),
            "got {err:?}"
        );
    }

    /// `StaticConfigSource` is now defined by the helper, and this pins that
    /// the extraction preserved its behaviour exactly, empty strings
    /// included.
    #[tokio::test]
    async fn static_source_matches_the_helper() {
        let data: HashMap<String, String> = [
            ("A".to_string(), "from-map".to_string()),
            ("EMPTY".to_string(), String::new()),
        ]
        .into_iter()
        .collect();
        let declared = [
            required("A", "unused-default"),
            required("EMPTY", "unused-default"),
            required("C", "c-default"),
            optional("D", ""),
        ];

        let source = StaticConfigSource::new(data.clone());
        let from_source = source
            .load_for_block("blk", &declared)
            .await
            .expect("resolves")
            .into_inner();
        let from_helper = resolve_declared("blk", &declared, |k| data.get(k).cloned())
            .expect("resolves")
            .into_inner();

        assert_eq!(from_source, from_helper);
        assert_eq!(from_source.get("A").map(String::as_str), Some("from-map"));
        assert_eq!(from_source.get("EMPTY").map(String::as_str), Some(""));
        assert_eq!(from_source.get("C").map(String::as_str), Some("c-default"));
        assert_eq!(from_source.get("D"), None);
    }
}
