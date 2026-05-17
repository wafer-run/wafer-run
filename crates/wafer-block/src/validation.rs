//! Block config validation report types.
//!
//! Returned by [`Context::validate_all_block_configs`] (and the
//! corresponding `Wafer::validate_all_block_configs` re-export) to surface
//! which registered blocks are missing required config keys. Used by
//! deploy-time gates such as wafer-site's `/_health` route.
//!
//! [`Context::validate_all_block_configs`]: crate::context::Context::validate_all_block_configs

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
