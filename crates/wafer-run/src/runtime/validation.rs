//! Runtime validators: block interface action checks and required-config presence checks.
//!
//! Pure functions — no mutation of runtime state. Called from `Wafer::resolve()`
//! (config presence) and `RuntimeContext::call_block()` (interface action).

use wafer_block::types::BlockInfo;

/// A single `(block, key)` pair whose required config value was not provided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingConfig {
    pub block_name: String,
    pub key: String,
}

/// Collect every required config key that is missing or empty, across the given blocks.
///
/// A `ConfigVar` is "required" when `default.is_empty() && !auto_generate`.
/// Config is passed as a `serde_json::Value` object (the shape stored in
/// `Wafer::block_configs`); presence is checked by reading the string-coerced
/// value for the declared key.
pub fn collect_missing_config<'a>(
    blocks: &'a [(BlockInfo, &'a serde_json::Value)],
) -> Vec<MissingConfig> {
    let mut out = Vec::new();
    for (info, cfg) in blocks {
        for cv in &info.config_keys {
            if !cv.default.is_empty() || cv.auto_generate {
                continue;
            }
            let provided = cfg
                .get(&cv.key)
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            if !provided {
                out.push(MissingConfig {
                    block_name: info.name.clone(),
                    key: cv.key.clone(),
                });
            }
        }
    }
    out
}

/// Format a list of `MissingConfig` entries into a single multi-block error message.
///
/// Output shape: `"missing required config: [block-1: KEY_A, KEY_B; block-2: KEY_C]"`.
pub fn format_missing_config(missing: &[MissingConfig]) -> String {
    use std::collections::BTreeMap;
    let mut by_block: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for m in missing {
        by_block.entry(&m.block_name).or_default().push(&m.key);
    }
    let parts: Vec<String> = by_block
        .into_iter()
        .map(|(block, keys)| format!("{block}: {}", keys.join(", ")))
        .collect();
    format!("missing required config: [{}]", parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wafer_block::types::{BlockInfo, ConfigVar};

    fn mk_block(name: &str, cfg_vars: Vec<ConfigVar>) -> BlockInfo {
        let mut info = BlockInfo::new(name, "0.1.0", "test@v1", "test");
        info.config_keys = cfg_vars;
        info
    }

    #[test]
    fn config_required_empty_no_default_missing() {
        let info = mk_block("org/a", vec![ConfigVar::new("ORG__A__KEY", "desc", "")]);
        let cfg = serde_json::json!({});
        let missing = collect_missing_config(&[(info, &cfg)]);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].block_name, "org/a");
        assert_eq!(missing[0].key, "ORG__A__KEY");
    }

    #[test]
    fn config_required_with_default_present() {
        let info = mk_block(
            "org/a",
            vec![ConfigVar::new("ORG__A__KEY", "desc", "fallback")],
        );
        let cfg = serde_json::json!({});
        assert!(collect_missing_config(&[(info, &cfg)]).is_empty());
    }

    #[test]
    fn config_auto_generate_skipped() {
        let mut cv = ConfigVar::new("ORG__A__SECRET", "desc", "");
        cv.auto_generate = true;
        let info = mk_block("org/a", vec![cv]);
        let cfg = serde_json::json!({});
        assert!(collect_missing_config(&[(info, &cfg)]).is_empty());
    }

    #[test]
    fn config_value_provided_passes() {
        let info = mk_block("org/a", vec![ConfigVar::new("ORG__A__KEY", "desc", "")]);
        let cfg = serde_json::json!({ "ORG__A__KEY": "supplied" });
        assert!(collect_missing_config(&[(info, &cfg)]).is_empty());
    }

    #[test]
    fn config_multiple_missing_aggregated() {
        let a = mk_block(
            "org/a",
            vec![
                ConfigVar::new("ORG__A__K1", "desc", ""),
                ConfigVar::new("ORG__A__K2", "desc", ""),
            ],
        );
        let b = mk_block("org/b", vec![ConfigVar::new("ORG__B__K1", "desc", "")]);
        let empty = serde_json::json!({});
        let missing = collect_missing_config(&[(a, &empty), (b, &empty)]);
        assert_eq!(missing.len(), 3);

        let rendered = format_missing_config(&missing);
        assert!(rendered.contains("org/a: ORG__A__K1, ORG__A__K2"));
        assert!(rendered.contains("org/b: ORG__B__K1"));
    }
}
