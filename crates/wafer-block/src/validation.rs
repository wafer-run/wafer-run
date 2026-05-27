//! Block config validation report types.
//!
//! Returned by [`Context::validate_all_block_configs`] (and the
//! corresponding `Wafer::validate_all_block_configs` re-export) to surface
//! which registered blocks are missing required config keys. Used by
//! deploy-time gates such as wafer-site's `/_health` route.
//!
//! [`Context::validate_all_block_configs`]: crate::context::Context::validate_all_block_configs

use crate::types::BlockInfo;

/// Outcome of validating every registered block's declared `ConfigVar`
/// against the active config source.
#[derive(Debug, Clone, Default)]
pub struct ValidationReport {
    /// Block names whose declared config keys all resolved successfully.
    /// Sorted lexicographically for deterministic output.
    pub ok: Vec<String>,
    /// Blocks with missing required keys or an unreachable config source.
    /// Sorted by `block` for deterministic output.
    pub broken: Vec<BrokenBlock>,
}

/// A single block that failed declared-key validation.
#[derive(Debug, Clone)]
pub struct BrokenBlock {
    /// Name of the block (e.g. `suppers-ai/auth`) that failed validation.
    pub block: String,
    /// Missing required keys. Currently carries at most one entry — the
    /// underlying `ConfigSource::load_for_block` short-circuits on the
    /// first miss. Widening to `Vec<String>` ahead of time keeps the
    /// JSON shape stable for callers (e.g. `/_health` response body).
    pub missing_keys: Vec<String>,
}

/// Returns JSON-config keys that appear in `data` (a JSON object) but
/// are not declared in `info.flow_config`. Empty if all keys are
/// declared, or if `data` is empty, not parseable as JSON, or not a
/// JSON object.
///
/// `flow_config` (not `config_keys`) is the right comparison slot:
/// [`BlockInfo::flow_config`] declares the snake_case JSON config keys
/// a block reads at `lifecycle(Init)` (or per-request via
/// `ctx.config_get`). [`BlockInfo::config_keys`] declares the separate
/// SCREAMING_SNAKE env-var slot, which is unrelated to the JSON event
/// payload checked here.
///
/// Returned keys are in `serde_json`'s iteration order over the parsed
/// object — callers should not depend on order beyond "stable across
/// runs of a single binary".
///
/// Used by blocks at `lifecycle(Init)` to warn about config typos
/// (would have caught the Wave 8/9 `allow_origins`-vs-`allowed_origins`
/// regression at deploy time).
pub fn unknown_flow_config_keys(info: &BlockInfo, data: &[u8]) -> Vec<String> {
    if data.is_empty() {
        return Vec::new();
    }
    let parsed: serde_json::Value = match serde_json::from_slice(data) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(obj) = parsed.as_object() else {
        return Vec::new();
    };
    let declared: std::collections::HashSet<&str> =
        info.flow_config.iter().map(|v| v.key.as_str()).collect();
    obj.keys()
        .filter(|k| !declared.contains(k.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod unknown_flow_config_keys_tests {
    use super::unknown_flow_config_keys;
    use crate::types::{BlockInfo, ConfigVar};

    fn info_with_flow_config(keys: &[&str]) -> BlockInfo {
        BlockInfo::new("test/block", "0.0.1", "test@v1", "test")
            .flow_config(keys.iter().map(|k| ConfigVar::new(k, "", "")).collect())
    }

    #[test]
    fn empty_data_returns_no_unknown_keys() {
        let info = info_with_flow_config(&["foo"]);
        assert_eq!(unknown_flow_config_keys(&info, &[]), Vec::<String>::new());
    }

    #[test]
    fn non_object_json_returns_no_unknown_keys() {
        let info = info_with_flow_config(&["foo"]);
        assert_eq!(
            unknown_flow_config_keys(&info, b"[1,2,3]"),
            Vec::<String>::new(),
        );
    }

    #[test]
    fn unparseable_json_returns_no_unknown_keys() {
        let info = info_with_flow_config(&["foo"]);
        assert_eq!(
            unknown_flow_config_keys(&info, b"not-json"),
            Vec::<String>::new(),
        );
    }

    #[test]
    fn all_declared_returns_no_unknown_keys() {
        let info = info_with_flow_config(&["foo", "bar"]);
        let data = br#"{"foo":1,"bar":2}"#;
        assert_eq!(unknown_flow_config_keys(&info, data), Vec::<String>::new(),);
    }

    #[test]
    fn mixed_returns_only_unknowns() {
        let info = info_with_flow_config(&["foo"]);
        let data = br#"{"foo":1,"baz":2}"#;
        assert_eq!(
            unknown_flow_config_keys(&info, data),
            vec!["baz".to_string()],
        );
    }

    #[test]
    fn all_unknown_returns_full_list_sorted_eq() {
        let info = info_with_flow_config(&["foo"]);
        let data = br#"{"baz":1,"qux":2}"#;
        let mut got = unknown_flow_config_keys(&info, data);
        got.sort();
        assert_eq!(got, vec!["baz".to_string(), "qux".to_string()]);
    }

    #[test]
    fn empty_object_returns_no_unknown_keys() {
        let info = info_with_flow_config(&["foo"]);
        assert_eq!(unknown_flow_config_keys(&info, b"{}"), Vec::<String>::new(),);
    }

    #[test]
    fn config_keys_slot_is_ignored() {
        let info = BlockInfo::new("test/block", "0.0.1", "test@v1", "test")
            .config_keys(vec![ConfigVar::new("TEST__BLOCK__FOO", "", "")]);
        let data = br#"{"TEST__BLOCK__FOO":1}"#;
        assert_eq!(
            unknown_flow_config_keys(&info, data),
            vec!["TEST__BLOCK__FOO".to_string()],
            "config_keys slot must not satisfy JSON key membership",
        );
    }
}
