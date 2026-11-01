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

    /// The source is unreachable, or answered with a condition that clears
    /// on its own: a backend down, overloaded or over a quota that resets,
    /// a network timeout, or a per-invocation database call budget this
    /// invocation has spent (`CALL_BUDGET_EXHAUSTED`: a fresh invocation may
    /// run the read). A source with a coded backend error chooses
    /// between this and [`SourceFault`](Self::SourceFault) with
    /// [`init_failure_is_transient`](crate::init_failure_is_transient), the
    /// rule `lifecycle(Init)` failures are classified by. Retrying can
    /// succeed, so the block's init fails with
    /// [`InitError::Transient`](crate::runtime::slot::InitError::Transient):
    /// not cached in the block slot.
    #[error("transient error fetching config for `{block}`: {source}")]
    Transient {
        /// Block name whose config load failed.
        block: String,
        /// Underlying transport error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The source answered with an error retrying cannot fix: its table is
    /// missing or malformed, a result it cannot interpret. The block's init
    /// fails with
    /// [`InitError::Permanent`](crate::runtime::slot::InitError::Permanent),
    /// cached for the slot's lifetime.
    #[error("config source fault for `{block}`: {source}")]
    SourceFault {
        /// Block name whose config load failed.
        block: String,
        /// The source's error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl ConfigError {
    /// The block's init failure for this config error: a missing required
    /// key and a source fault are permanent (retrying reads the same
    /// answer), a transient source error is transient.
    pub(crate) fn into_init_error(self) -> crate::runtime::slot::InitError {
        use crate::runtime::slot::InitError;
        match self {
            Self::MissingRequired { block, key } => InitError::Permanent(format!(
                "block `{block}` missing required config key `{key}`"
            )),
            Self::Transient { source, .. } => {
                InitError::Transient(format!("config fetch failed: {source}"))
            }
            Self::SourceFault { source, .. } => {
                InitError::Permanent(format!("config source fault: {source}"))
            }
        }
    }
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
    /// - Return [`ConfigError::Transient`] when the source is unreachable or
    ///   answers with a condition that clears (network, a backend down or
    ///   over a resetting quota, a spent per-invocation call budget).
    /// - Return [`ConfigError::SourceFault`] when the source answers with an
    ///   error retrying cannot fix (a missing table, an answer it cannot
    ///   interpret).
    /// - For a backend error that carries an error code (and detail code),
    ///   choose between the two with
    ///   [`init_failure_is_transient`](crate::init_failure_is_transient), so
    ///   the same refusal is classified the same way here as in a block's
    ///   `lifecycle(Init)`.
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
/// missing required keys ([`ValidationReport::broken`](wafer_block::ValidationReport))
/// and which could not be checked because the source failed
/// ([`ValidationReport::unavailable`](wafer_block::ValidationReport), with
/// the failure's class; the source's error itself is logged, never put in
/// the report).
///
/// # One log line per call
///
/// Source failures are logged once per call, after the walk, as a single
/// summary line (see [`log_unavailable_summary`]): `ERROR` when any block
/// met a [`SourceFault`](ConfigError::SourceFault), otherwise `WARN`. A
/// source that is down fails every config-declaring block with the same
/// error, and a health endpoint calls this on every hit, so a line per
/// block would repeat one error text once per block per request. The
/// summary names every affected block with its class and carries each
/// distinct error text once. The per-block detail callers act on stays in
/// the report.
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
    use wafer_block::{BrokenBlock, UnavailableBlock, UnavailableReason, ValidationReport};

    let mut report = ValidationReport::default();
    let mut failures: Vec<SourceFailure> = Vec::new();
    for (name, block) in blocks {
        let info = block.info();
        match source.load_for_block(name, &info.config_keys).await {
            Ok(_) => report.ok.push(name.clone()),
            Err(ConfigError::MissingRequired { block, key }) => {
                report.broken.push(BrokenBlock {
                    block,
                    missing_keys: vec![key],
                });
            }
            Err(ConfigError::Transient { block, source }) => failures.push(SourceFailure {
                block,
                reason: UnavailableReason::Transient,
                error: source.to_string(),
            }),
            Err(ConfigError::SourceFault { block, source }) => failures.push(SourceFailure {
                block,
                reason: UnavailableReason::SourceFault,
                error: source.to_string(),
            }),
        }
    }
    failures.sort_by(|a, b| a.block.cmp(&b.block));
    report.unavailable = failures
        .iter()
        .map(|f| UnavailableBlock {
            block: f.block.clone(),
            reason: f.reason,
        })
        .collect();
    log_unavailable_summary(failures);
    report.ok.sort();
    report.broken.sort_by(|a, b| a.block.cmp(&b.block));
    report
}

/// One block the config source failed for, with the source's error text.
/// [`validate_block_configs`] builds the report's `unavailable` entries and
/// its summary log line from these.
struct SourceFailure {
    block: String,
    reason: wafer_block::UnavailableReason,
    error: String,
}

/// Render the summary of a validation call's source failures, or `None`
/// when there were none.
///
/// Failures that share a class and an error text form one group, so each
/// distinct text appears once, followed by the blocks it hit:
///
/// ```text
/// source_fault (org/c): no such table: variables; transient (org/a, org/b): D1_ERROR: network connection lost
/// ```
///
/// Groups are ordered by class (`source_fault` before `transient`), then by
/// error text; the blocks in a group are sorted. The order is fixed so the
/// same outage writes the same line on every call. The error text comes
/// last in its group because it is the source's own text and may contain
/// any punctuation.
///
/// `failures` arrives sorted by block, as [`validate_block_configs`] sorts
/// it for the report.
fn summarize_unavailable(failures: Vec<SourceFailure>) -> Option<UnavailableSummary> {
    use std::collections::BTreeMap;

    if failures.is_empty() {
        return None;
    }
    let blocks = failures.len();
    let source_faults = failures
        .iter()
        .filter(|f| f.reason == wafer_block::UnavailableReason::SourceFault)
        .count();
    let mut groups: BTreeMap<(&'static str, String), Vec<String>> = BTreeMap::new();
    for f in failures {
        groups
            .entry((f.reason.as_str(), f.error))
            .or_default()
            .push(f.block);
    }
    let failures = groups
        .into_iter()
        .map(|((reason, error), blocks)| format!("{reason} ({}): {error}", blocks.join(", ")))
        .collect::<Vec<_>>()
        .join("; ");
    Some(UnavailableSummary {
        blocks,
        source_faults,
        failures,
    })
}

/// A validation call's source failures, rendered for one log line.
struct UnavailableSummary {
    /// How many blocks the source failed for.
    blocks: usize,
    /// How many of them met a source fault (the rest were transient).
    source_faults: usize,
    /// The grouped failures; see [`summarize_unavailable`].
    failures: String,
}

/// Write one log line for a validation call's source failures, if it had
/// any: `ERROR` when any block met a source fault, otherwise `WARN`. The
/// levels are the ones the per-block lines had.
fn log_unavailable_summary(failures: Vec<SourceFailure>) {
    let Some(summary) = summarize_unavailable(failures) else {
        return;
    };
    let UnavailableSummary {
        blocks,
        source_faults,
        failures,
    } = summary;
    if source_faults > 0 {
        tracing::error!(
            blocks,
            source_faults,
            %failures,
            "config validation: config source failed"
        );
    } else {
        tracing::warn!(
            blocks,
            %failures,
            "config validation: config source unreachable"
        );
    }
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

#[cfg(test)]
mod validate_block_configs_tests {
    use wafer_block::{
        context::Context, types::BlockInfo, InputStream, LifecycleEvent, Message, OutputStream,
        UnavailableBlock, UnavailableReason, WaferError,
    };

    use super::*;

    /// A block whose only job is to declare one required config key.
    struct Declares(&'static str);

    #[wafer_async_trait]
    impl wafer_block::Block for Declares {
        fn info(&self) -> BlockInfo {
            let mut info = BlockInfo::new(self.0, "0.0.1", "test/iface@v1", "declares a key");
            info.config_keys = vec![ConfigVar::new("TEST__KEY", "a key", "")];
            info
        }
        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            OutputStream::respond(vec![])
        }
        async fn lifecycle(
            &self,
            _ctx: &dyn Context,
            _event: LifecycleEvent,
        ) -> Result<(), WaferError> {
            Ok(())
        }
    }

    /// Answers each block with its scripted outcome; unscripted blocks resolve.
    #[derive(Default)]
    struct Scripted(HashMap<&'static str, (UnavailableReason, &'static str)>);

    #[wafer_async_trait]
    impl ConfigSource for Scripted {
        async fn load_for_block(
            &self,
            block: &str,
            _declared_keys: &[ConfigVar],
        ) -> Result<EnvBlockConfig, ConfigError> {
            match self.0.get(block) {
                None => Ok(EnvBlockConfig::default()),
                Some((UnavailableReason::Transient, text)) => Err(ConfigError::Transient {
                    block: block.to_string(),
                    source: (*text).into(),
                }),
                Some((UnavailableReason::SourceFault, text)) => Err(ConfigError::SourceFault {
                    block: block.to_string(),
                    source: (*text).into(),
                }),
            }
        }
    }

    async fn validate(names: &[&'static str], script: Scripted) -> wafer_block::ValidationReport {
        let blocks: Vec<(String, Arc<dyn wafer_block::Block>)> = names
            .iter()
            .map(|n| {
                (
                    n.to_string(),
                    Arc::new(Declares(n)) as Arc<dyn wafer_block::Block>,
                )
            })
            .collect();
        let source: Arc<dyn ConfigSource> = Arc::new(script);
        validate_block_configs(blocks.iter().map(|(n, b)| (n, b)), &source).await
    }

    const OUTAGE: &str = "D1_ERROR: Network connection lost.";
    const OVERLOAD: &str = "D1_ERROR: D1 DB is overloaded.";
    const NO_TABLE: &str = "no such table: variables";

    /// The validator's lines. A level is matched with its surrounding
    /// spaces (`" ERROR "`) because the D1 texts contain `D1_ERROR`.
    fn summary_lines<'a>(lines: &[&'a str]) -> Vec<&'a str> {
        lines
            .iter()
            .copied()
            .filter(|l| l.contains("config validation"))
            .collect()
    }

    /// N failing blocks write ONE line, at `ERROR` because one of them met a
    /// source fault. The line names every failing block with its class and
    /// carries each distinct error text exactly once. The report keeps the
    /// per-block entries.
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn a_call_with_many_failing_blocks_logs_one_line_with_every_block_and_text() {
        let names = [
            "test/a-down",
            "test/b-down",
            "test/c-down",
            "test/d-overloaded",
            "test/e-fault",
            "test/f-healthy",
        ];
        let script = Scripted(
            [
                ("test/a-down", (UnavailableReason::Transient, OUTAGE)),
                ("test/b-down", (UnavailableReason::Transient, OUTAGE)),
                ("test/c-down", (UnavailableReason::Transient, OUTAGE)),
                (
                    "test/d-overloaded",
                    (UnavailableReason::Transient, OVERLOAD),
                ),
                ("test/e-fault", (UnavailableReason::SourceFault, NO_TABLE)),
            ]
            .into_iter()
            .collect(),
        );

        let report = validate(&names, script).await;

        let unavailable = |block: &str, reason| UnavailableBlock {
            block: block.to_string(),
            reason,
        };
        assert_eq!(
            report.unavailable,
            vec![
                unavailable("test/a-down", UnavailableReason::Transient),
                unavailable("test/b-down", UnavailableReason::Transient),
                unavailable("test/c-down", UnavailableReason::Transient),
                unavailable("test/d-overloaded", UnavailableReason::Transient),
                unavailable("test/e-fault", UnavailableReason::SourceFault),
            ]
        );
        assert_eq!(report.ok, vec!["test/f-healthy".to_string()]);

        logs_assert(|lines: &[&str]| {
            let summary = summary_lines(lines);
            let [line] = summary.as_slice() else {
                return Err(format!(
                    "expected one line, got {}: {summary:#?}",
                    summary.len()
                ));
            };
            if !line.contains(" ERROR ") {
                return Err(format!("a source fault logs at ERROR: {line}"));
            }
            for text in [OUTAGE, OVERLOAD, NO_TABLE] {
                let n = line.matches(text).count();
                if n != 1 {
                    return Err(format!("`{text}` appears {n} times, expected once: {line}"));
                }
            }
            for group in [
                format!("source_fault (test/e-fault): {NO_TABLE}"),
                format!("transient (test/a-down, test/b-down, test/c-down): {OUTAGE}"),
                format!("transient (test/d-overloaded): {OVERLOAD}"),
            ] {
                if !line.contains(&group) {
                    return Err(format!("missing `{group}`: {line}"));
                }
            }
            if line.contains("test/f-healthy") {
                return Err(format!("a healthy block is not a failure: {line}"));
            }
            if !line.contains("blocks=5") || !line.contains("source_faults=1") {
                return Err(format!("expected the counts: {line}"));
            }
            Ok(())
        });
    }

    /// Only transient failures: one `WARN` line, no `ERROR`.
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn transient_failures_alone_log_one_warn_line() {
        let script = Scripted(
            [
                ("test/a", (UnavailableReason::Transient, OUTAGE)),
                ("test/b", (UnavailableReason::Transient, OUTAGE)),
            ]
            .into_iter()
            .collect(),
        );
        validate(&["test/a", "test/b"], script).await;

        logs_assert(|lines: &[&str]| {
            let summary = summary_lines(lines);
            let [line] = summary.as_slice() else {
                return Err(format!(
                    "expected one line, got {}: {summary:#?}",
                    summary.len()
                ));
            };
            if !line.contains(" WARN ") || line.contains(" ERROR ") {
                return Err(format!("transient-only failures log at WARN: {line}"));
            }
            if !line.contains(&format!("transient (test/a, test/b): {OUTAGE}")) {
                return Err(format!("missing the group: {line}"));
            }
            Ok(())
        });
    }

    /// A call the source answers for every block writes nothing.
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn a_call_without_source_failures_logs_nothing() {
        let report = validate(&["test/a", "test/b"], Scripted::default()).await;
        assert!(report.unavailable.is_empty());
        logs_assert(|lines: &[&str]| {
            let summary = summary_lines(lines);
            if summary.is_empty() {
                Ok(())
            } else {
                Err(format!("expected no lines: {summary:#?}"))
            }
        });
    }
}
