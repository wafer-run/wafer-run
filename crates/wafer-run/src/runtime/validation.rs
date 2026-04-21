//! Runtime validators: block interface action checks and required-config presence checks.
//!
//! Pure functions — no mutation of runtime state. Called from `Wafer::resolve()`
//! (config presence) and `RuntimeContext::call_block()` (interface action).

use std::{collections::HashSet, sync::Mutex};

use wafer_block::types::{BlockInfo, InterfaceSpec};

/// A single `(block, key)` pair whose required config value was not provided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingConfig {
    pub block_name: String,
    pub key: String,
}

/// Collect every required config key that is missing or empty, across the given blocks.
///
/// A `ConfigVar` is "required" when `default.is_empty() && !auto_generate && !optional`.
/// Optional vars (`optional == true`) are admin-configurable-later and skipped by this
/// check — the block degrades gracefully when they are absent.
/// Config is passed as a `serde_json::Value` object (the shape stored in
/// `Wafer::block_configs`); presence is checked by reading the string-coerced
/// value for the declared key.
pub fn collect_missing_config<'a>(
    blocks: &'a [(BlockInfo, &'a serde_json::Value)],
) -> Vec<MissingConfig> {
    let mut out = Vec::new();
    for (info, cfg) in blocks {
        for cv in &info.config_keys {
            if !cv.default.is_empty() || cv.auto_generate || cv.optional {
                continue;
            }
            let provided = cfg.get(&cv.key).is_some_and(|v| match v {
                serde_json::Value::Null => false,
                serde_json::Value::String(s) => !s.is_empty(),
                _ => true,
            });
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

/// Result of checking whether an action is valid for a block's declared interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionCheck {
    /// Action is valid for the block's interface.
    Valid,
    /// The action is not listed in the interface's action map.
    ///
    /// Message is pre-formatted for use in a `WaferError::invalid_argument`.
    Invalid { message: String },
    /// The block's interface string does not match any registered `InterfaceSpec`.
    ///
    /// Caller should warn-once and then treat the call as valid (backward compat
    /// for custom interfaces).
    UnknownInterface,
}

/// Check whether `action` is part of the action map for the block's declared interface.
///
/// Rules:
/// - If the interface has an **empty** action map, it is action-agnostic
///   (e.g., `middleware@v1`): any action is valid.
/// - If the interface has a non-empty action map, `action` must be a key in it.
/// - If the interface name matches no registered `InterfaceSpec`, return
///   `UnknownInterface` so the caller can warn-once and proceed.
pub fn check_action_interface(
    block_name: &str,
    interface_name: &str,
    action: &str,
    specs: &[InterfaceSpec],
) -> ActionCheck {
    let Some(spec) = specs.iter().find(|s| s.name == interface_name) else {
        return ActionCheck::UnknownInterface;
    };
    if spec.actions.is_empty() {
        return ActionCheck::Valid;
    }
    if spec.actions.contains_key(action) {
        return ActionCheck::Valid;
    }
    ActionCheck::Invalid {
        message: format!(
            "block '{block_name}' with interface '{interface_name}' does not expose action '{action}'"
        ),
    }
}

/// Emit a `WARN`-level log line exactly once per `(block_name)` for the
/// lifetime of the `warned` set.
///
/// Called from `RuntimeContext::call_block()` when a target block declares
/// an interface name that isn't in the runtime's registered `InterfaceSpec`
/// set. Preserves backward compatibility for custom interfaces while
/// signalling to the block author that action validation isn't catching
/// mistakes for them.
pub fn warn_once_unknown_interface(
    warned: &Mutex<HashSet<String>>,
    block_name: &str,
    interface_name: &str,
) {
    let mut guard = warned.lock().expect("warn-once mutex poisoned");
    if guard.insert(block_name.to_string()) {
        tracing::warn!(
            block = %block_name,
            interface = %interface_name,
            "block declares unknown interface; skipping action validation"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use wafer_block::types::{ActionSpec, BlockInfo, ConfigVar, InterfaceSpec};

    use super::*;

    fn mk_block(name: &str, cfg_vars: Vec<ConfigVar>) -> BlockInfo {
        let mut info = BlockInfo::new(name, "0.1.0", "test@v1", "test");
        info.config_keys = cfg_vars;
        info
    }

    fn db_interface() -> InterfaceSpec {
        let mut actions = HashMap::new();
        actions.insert(
            "retrieve".into(),
            ActionSpec {
                description: "".into(),
                message_schema: None,
                response_schema: None,
            },
        );
        actions.insert(
            "list".into(),
            ActionSpec {
                description: "".into(),
                message_schema: None,
                response_schema: None,
            },
        );
        InterfaceSpec {
            name: "database@v1".into(),
            description: "".into(),
            actions,
        }
    }

    #[test]
    fn interface_valid_action() {
        let specs = vec![db_interface()];
        let result = check_action_interface("org/sqlite", "database@v1", "retrieve", &specs);
        assert!(matches!(result, ActionCheck::Valid));
    }

    #[test]
    fn interface_unknown_action_rejected() {
        let specs = vec![db_interface()];
        let result = check_action_interface("org/sqlite", "database@v1", "publish", &specs);
        match result {
            ActionCheck::Invalid { message } => {
                assert!(message.contains("org/sqlite"));
                assert!(message.contains("database@v1"));
                assert!(message.contains("publish"));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn interface_action_agnostic_interface_passes_any() {
        let mw = InterfaceSpec {
            name: "middleware@v1".into(),
            description: "".into(),
            actions: HashMap::new(),
        };
        let specs = vec![mw];
        assert_eq!(
            check_action_interface("org/cors", "middleware@v1", "anything", &specs),
            ActionCheck::Valid
        );
    }

    #[test]
    fn interface_unknown_interface_returns_unknown() {
        let specs = vec![db_interface()];
        assert_eq!(
            check_action_interface("org/x", "my-org/custom@v1", "retrieve", &specs),
            ActionCheck::UnknownInterface
        );
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

    #[test]
    #[tracing_test::traced_test]
    fn warn_once_unknown_interface_emits_exactly_one_line() {
        let warned: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
        warn_once_unknown_interface(&warned, "org/weird", "my-org/custom@v1");
        warn_once_unknown_interface(&warned, "org/weird", "my-org/custom@v1");
        warn_once_unknown_interface(&warned, "org/weird", "my-org/custom@v1");

        // Expect presence.
        assert!(logs_contain("org/weird"));

        // Expect exactly one matching log line.
        logs_assert(|lines: &[&str]| {
            let n = lines.iter().filter(|l| l.contains("org/weird")).count();
            if n == 1 {
                Ok(())
            } else {
                Err(format!("expected 1 warning line, got {n}"))
            }
        });
    }

    #[test]
    fn config_non_string_value_counts_as_present() {
        let info = mk_block(
            "org/a",
            vec![
                ConfigVar::new("ORG__A__PORT", "port", ""),
                ConfigVar::new("ORG__A__DEBUG", "debug", ""),
                ConfigVar::new("ORG__A__NESTED", "nested", ""),
            ],
        );
        let cfg = serde_json::json!({
            "ORG__A__PORT": 8080,
            "ORG__A__DEBUG": true,
            "ORG__A__NESTED": { "key": "v" },
        });
        assert!(collect_missing_config(&[(info, &cfg)]).is_empty());
    }

    #[test]
    fn config_null_value_counts_as_missing() {
        let info = mk_block("org/a", vec![ConfigVar::new("ORG__A__KEY", "k", "")]);
        let cfg = serde_json::json!({ "ORG__A__KEY": null });
        let missing = collect_missing_config(&[(info, &cfg)]);
        assert_eq!(missing.len(), 1);
    }

    #[test]
    fn config_optional_skipped() {
        let mut cv = ConfigVar::new("ORG__A__KEY", "desc", "");
        cv.optional = true;
        let info = mk_block("org/a", vec![cv]);
        let cfg = serde_json::json!({});
        assert!(collect_missing_config(&[(info, &cfg)]).is_empty());
    }
}
