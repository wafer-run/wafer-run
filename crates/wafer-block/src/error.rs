//! Typed error types for the WAFER runtime.
//!
//! Replaces the previous `Result<T, String>` pattern with structured errors
//! that callers can match on programmatically.

/// Errors that can occur during WAFER runtime operations.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    // ── Block registration ──────────────────────────────────────────────
    /// A block with the same name is already registered.
    #[error("block '{name}' already registered")]
    DuplicateBlock {
        /// The conflicting block name.
        name: String,
    },

    /// Block name does not follow the `{org}/{block}` naming convention.
    #[error("invalid block name '{name}': {reason}")]
    InvalidBlockName {
        /// The offending name.
        name: String,
        /// Why the name was rejected.
        reason: String,
    },

    /// A block's config var doesn't match its expected prefix.
    #[error("block '{name}' declares config var '{var}' which doesn't match prefix '{prefix}'")]
    ConfigVarPrefix {
        /// Block name.
        name: String,
        /// The declared config key.
        var: String,
        /// The expected `{ORG}__{BLOCK}__` prefix.
        prefix: String,
    },

    // ── Block lifecycle ─────────────────────────────────────────────────
    /// A block failed during initialization (lifecycle start).
    #[error("block '{name}' init failed: {reason}")]
    BlockInit {
        /// Block name.
        name: String,
        /// Cause of the init failure.
        reason: String,
    },

    // ── Configuration ───────────────────────────────────────────────────
    /// Configuration error (parsing, serialization, file I/O).
    #[error("config error: {0}")]
    Config(String),

    // ── Flow ────────────────────────────────────────────────────────────
    /// Flow parse or validation error.
    #[error("flow error: {0}")]
    Flow(String),

    // ── WASM ────────────────────────────────────────────────────────────
    /// WASM compilation, linking, instantiation, or guest execution error.
    #[error("WASM error: {0}")]
    Wasm(String),

    // ── Registry / remote blocks ────────────────────────────────────────
    /// Error fetching, parsing, or resolving a remote block from the registry.
    #[error("registry error: {0}")]
    Registry(String),

    /// Error loading blocks from `wafer.lock` + cache (Path B).
    /// Carries a formatted message from the internal `LockLoaderError`
    /// in wafer-run (structured upstream, flattened here to keep wafer-block
    /// free of wafer-run / toml / wasmi deps).
    #[error("lockfile error: {0}")]
    Lockfile(String),

    /// The remote block requires a newer ABI version than the runtime supports.
    #[error("ABI mismatch for block '{name}': requires {required}, runtime supports {supported}")]
    AbiMismatch {
        /// Block name.
        name: String,
        /// ABI version the block was built against.
        required: u32,
        /// Highest ABI version this runtime understands.
        supported: u32,
    },

    /// Inventory registration of a block failed.
    #[error("inventory registration of '{name}' failed: {source}")]
    Inventory {
        /// Block name whose `inventory!` entry failed to register.
        name: String,
        /// Underlying registration failure.
        #[source]
        source: Box<RuntimeError>,
    },

    // ── Grant validation ────────────────────────────────────────────────
    /// One or more block grant declarations were rejected during validation.
    /// `Wafer::start()` returns this instead of silently dropping the grants.
    /// Remediation: relocate the grants to the block configured via
    /// `Wafer::set_admin_block(...)` or remove them.
    #[error(
        "{} typed grant(s) rejected:\n{}",
        .0.len(),
        .0.iter()
            .map(|e| format!("  - block `{}`: {}", e.block, e.reason))
            .collect::<Vec<_>>()
            .join("\n")
    )]
    GrantsRejected(Vec<GrantValidationError>),

    // ── Block resolution (seal-time) ────────────────────────────────────
    /// One or more block references could not be resolved during
    /// `seal()`. Aggregated across all flow steps and router routes;
    /// the embedded `Vec` always has at least one entry.
    ///
    /// Single-block runtime lookups (lazy init, flow runner) surface
    /// missing blocks as `WaferError::NOT_FOUND` rather than a typed
    /// `RuntimeError` variant; `BlocksNotFound` is reserved for the
    /// seal-time aggregator and carries source information so
    /// operators can find the link-graph cause inline.
    #[error(
        "{} referenced block(s) not found:\n{}",
        .0.len(),
        .0.iter()
            .map(|e| format!(
                "  - `{}`\n{}",
                e.name,
                e.sources.iter().map(render_source).collect::<Vec<_>>().join("\n"),
            ))
            .collect::<Vec<_>>()
            .join("\n")
    )]
    BlocksNotFound(Vec<BlockReferenceError>),

    // ── Catch-all ───────────────────────────────────────────────────────
    /// An error that doesn't fit any specific category.
    #[error("{0}")]
    Other(String),
}

/// Detail of a single grant-validation rejection from
/// `validate_and_collect_grants_for_block`. Aggregated into
/// `RuntimeError::GrantsRejected` so `Wafer::start()` can refuse boot
/// with all rejections listed in one error.
#[derive(Debug, Clone)]
pub struct GrantValidationError {
    /// Block that declared the rejected grant.
    pub block: String,
    /// The grant that was rejected.
    pub grant: crate::types::ResourceGrant,
    /// Human-readable reason (e.g. "typed Storage grants may only be declared by the admin block").
    pub reason: String,
}

/// Rejection reasons surfaced by [`crate::registry::BlockRegistry::add_alias`]
/// when registering an alias would violate the depth-1 invariant on the
/// alias map.
#[derive(Debug, thiserror::Error)]
pub enum AliasError {
    /// `alias == target` — self-loop rejected.
    #[error("alias `{alias}` cannot target itself")]
    Cycle {
        /// The alias name that was registered against itself.
        alias: String,
    },
    /// `target` is already a key in the alias map; registering this would
    /// chain `alias → target → existing_target`.
    #[error("alias `{alias}` cannot target `{target}` because `{target}` is itself an alias")]
    TargetIsAlias {
        /// The alias name that was attempted to be registered.
        alias: String,
        /// The target name that is already an alias key.
        target: String,
    },
    /// `alias` is already the target of some other alias; registering this
    /// would chain `other_alias → alias → target`.
    #[error(
        "alias `{alias}` cannot be registered because it is already the target of another alias"
    )]
    AliasIsExistingTarget {
        /// The alias name whose registration was rejected.
        alias: String,
    },
}

/// Where a missing block was referenced from. One source per entry on
/// [`BlockReferenceError::sources`]; the same missing block name can have
/// multiple sources (e.g. both a flow step and a router route).
///
/// Used inside [`RuntimeError::BlocksNotFound`] so operators can see the
/// link-graph cause of each missing block at boot time instead of having
/// to grep config files.
#[derive(Debug, Clone)]
pub enum BlockReferenceSource {
    /// Block was referenced from a flow step.
    Flow {
        /// Flow id (`flow.id`) that contains the step.
        flow_id: String,
        /// Zero-based index of the step in `flow.steps`.
        step_index: usize,
        /// The step's `id` field (mandatory on `wafer_flow::Step`).
        step_id: String,
    },
    /// Block was referenced from a `wafer-run/router` route entry.
    ///
    /// Emitted by `wafer-run`'s `runtime::router_walk` module when it
    /// detects an unresolvable `block` field on a parsed route entry.
    /// Tracked in `wafer-block` so the variant lives next to its
    /// container [`RuntimeError::BlocksNotFound`]; the emitter lives in
    /// `wafer-run`.
    RouterRoute {
        /// Block-config key holding the routes array. Equals
        /// `"wafer-run/router"` for the canonical router; equals the
        /// alias name when the router was registered under an alias.
        router_block: String,
        /// `path` field from the route entry.
        path: String,
        /// `actions`/`methods` field from the route entry, stored as
        /// the operator wrote them (no normalization). The router crate
        /// normalizes these for matching; for diagnostics we want the
        /// original strings so the error message matches the config.
        /// Empty when no actions/methods were configured.
        actions: Vec<String>,
    },
}

/// One missing-block entry in [`RuntimeError::BlocksNotFound`].
/// Aggregated by `Wafer::seal()` so a single error lists every missing
/// reference.
#[derive(Debug, Clone)]
pub struct BlockReferenceError {
    /// Canonical block name (post-alias-resolution) that was referenced
    /// but not registered and not downloadable from the registry.
    pub name: String,
    /// Every place that referenced this missing block.
    pub sources: Vec<BlockReferenceSource>,
}

fn render_source(src: &BlockReferenceSource) -> String {
    match src {
        BlockReferenceSource::Flow {
            flow_id,
            step_index,
            step_id,
        } => format!("      \u{2022} flow `{flow_id}` step {step_index} (`{step_id}`)"),
        BlockReferenceSource::RouterRoute {
            router_block,
            path,
            actions,
        } if actions.is_empty() => {
            format!("      \u{2022} router `{router_block}` route {path}")
        }
        BlockReferenceSource::RouterRoute {
            router_block,
            path,
            actions,
        } => format!(
            "      \u{2022} router `{router_block}` route {} {path}",
            actions.join(" ")
        ),
    }
}

impl From<String> for RuntimeError {
    fn from(s: String) -> Self {
        RuntimeError::Other(s)
    }
}

impl From<&str> for RuntimeError {
    fn from(s: &str) -> Self {
        RuntimeError::Other(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_variant_renders() {
        // Use Lockfile(String) as the inner because PR β already added it,
        // so its shape is stable. Any other RuntimeError variant works equally.
        let inner = RuntimeError::Lockfile("collision".into());
        let e = RuntimeError::Inventory {
            name: "acme/widget".into(),
            source: Box::new(inner),
        };
        let s = e.to_string();
        assert!(
            s.contains("acme/widget"),
            "name should appear in display: {s}"
        );
        assert!(s.contains("inventory"), "should mention inventory: {s}");
    }

    #[test]
    fn grants_rejected_display_lists_all_blocks() {
        use crate::types::ResourceGrant;

        let err = RuntimeError::GrantsRejected(vec![
            GrantValidationError {
                block: "suppers-ai/files".into(),
                grant: ResourceGrant::read_write("suppers-ai/files", "*"),
                reason: "typed Storage grants may only be declared by the admin block".into(),
            },
            GrantValidationError {
                block: "example/foo".into(),
                grant: ResourceGrant::read_write("foo", "https://api.example.com"),
                reason: "typed Network grants may only be declared by the admin block".into(),
            },
        ]);
        let display = format!("{err}");
        assert!(
            display.contains("2 typed grant(s) rejected"),
            "display: {display}"
        );
        assert!(display.contains("suppers-ai/files"), "display: {display}");
        assert!(display.contains("example/foo"), "display: {display}");
        assert!(display.contains("typed Storage"), "display: {display}");
        assert!(display.contains("typed Network"), "display: {display}");
    }

    #[test]
    fn alias_error_display_messages() {
        let cycle = super::AliasError::Cycle {
            alias: "x".to_string(),
        };
        let target_is_alias = super::AliasError::TargetIsAlias {
            alias: "a".to_string(),
            target: "b".to_string(),
        };
        let alias_is_existing_target = super::AliasError::AliasIsExistingTarget {
            alias: "y".to_string(),
        };

        assert_eq!(cycle.to_string(), "alias `x` cannot target itself");
        assert_eq!(
            target_is_alias.to_string(),
            "alias `a` cannot target `b` because `b` is itself an alias",
        );
        assert_eq!(
            alias_is_existing_target.to_string(),
            "alias `y` cannot be registered because it is already the target of another alias",
        );
    }

    #[test]
    fn blocks_not_found_renders_aggregated_message() {
        let err = RuntimeError::BlocksNotFound(vec![
            BlockReferenceError {
                name: "org/missing-a".to_string(),
                sources: vec![
                    BlockReferenceSource::Flow {
                        flow_id: "my-flow".to_string(),
                        step_index: 2,
                        step_id: "call-thing".to_string(),
                    },
                    BlockReferenceSource::RouterRoute {
                        router_block: "wafer-run/router".to_string(),
                        path: "/users/{id}".to_string(),
                        actions: vec!["GET".to_string()],
                    },
                ],
            },
            BlockReferenceError {
                name: "org/missing-b".to_string(),
                sources: vec![BlockReferenceSource::RouterRoute {
                    router_block: "my-router".to_string(),
                    path: "/x".to_string(),
                    actions: vec![],
                }],
            },
        ]);
        let msg = format!("{err}");
        assert!(
            msg.starts_with("2 referenced block(s) not found:"),
            "got: {msg}"
        );
        assert!(msg.contains("- `org/missing-a`"), "got: {msg}");
        assert!(
            msg.contains("flow `my-flow` step 2 (`call-thing`)"),
            "got: {msg}"
        );
        assert!(
            msg.contains("router `wafer-run/router` route GET /users/{id}"),
            "got: {msg}"
        );
        assert!(msg.contains("- `org/missing-b`"), "got: {msg}");
        assert!(msg.contains("router `my-router` route /x"), "got: {msg}");
    }
}
