//! Typed error types for the WAFER runtime.
//!
//! Replaces the previous `Result<T, String>` pattern with structured errors
//! that callers can match on programmatically.

/// Errors that can occur during WAFER runtime operations.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    // ── Block registration ──────────────────────────────────────────────
    /// A referenced block was not found in the registry.
    #[error("block '{name}' not found")]
    BlockNotFound {
        /// Block name that was looked up but not registered.
        name: String,
    },

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
}
