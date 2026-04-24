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
    BlockNotFound { name: String },

    /// A block with the same name is already registered.
    #[error("block '{name}' already registered")]
    DuplicateBlock { name: String },

    /// Block name does not follow the `{org}/{block}` naming convention.
    #[error("invalid block name '{name}': {reason}")]
    InvalidBlockName { name: String, reason: String },

    /// A block's config var doesn't match its expected prefix.
    #[error("block '{name}' declares config var '{var}' which doesn't match prefix '{prefix}'")]
    ConfigVarPrefix {
        name: String,
        var: String,
        prefix: String,
    },

    // ── Block lifecycle ─────────────────────────────────────────────────
    /// A block failed during initialization (lifecycle start).
    #[error("block '{name}' init failed: {reason}")]
    BlockInit { name: String, reason: String },

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
        name: String,
        required: u32,
        supported: u32,
    },

    // ── Catch-all ───────────────────────────────────────────────────────
    /// An error that doesn't fit any specific category.
    #[error("{0}")]
    Other(String),
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
