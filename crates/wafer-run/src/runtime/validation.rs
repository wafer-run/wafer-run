//! Runtime validators: block interface action checks.
//!
//! Pure functions — no mutation of runtime state. Called from
//! `RuntimeContext::call_block()`. Required-config presence checks live in
//! `runtime/config_source.rs::validate_block_configs` (the `ConfigSource`-
//! driven validator behind `Wafer::validate_all_block_configs`).

use std::collections::HashSet;

use parking_lot::Mutex;
use wafer_block::types::InterfaceSpec;

/// Result of checking whether an action is valid for a block's declared interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionCheck {
    /// Action is valid for the block's interface.
    Valid,
    /// The action is not listed in the interface's action map.
    ///
    /// Message is pre-formatted for use in a `WaferError::invalid_argument`.
    Invalid {
        /// Pre-formatted, human-readable error message suitable for surfacing to callers.
        message: String,
    },
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
    let mut guard = warned.lock();
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

    use wafer_block::types::{ActionSpec, InterfaceSpec};

    use super::*;

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
}
