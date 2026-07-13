//! Pluggable async loader for external WASM/JS assets declared via
//! `BlockInfo::external_assets`. The wasm import `__wafer_host_load_asset`
//! suspends the guest and resumes it once the host's `LoadAssetCallback`
//! returns.
//!
//! See `docs/superpowers/specs/2026-04-18-gizza-ai-design.md` §
//! "External asset loading (host side)".

use std::fmt;

use wafer_block_macro::wafer_async_trait;

/// Outcome of an asset-load attempt as reported back to the WASM guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetLoadStatus {
    /// Asset is loaded and ready to be invoked.
    Ready,
    /// Loading has been requested but is not finished.
    Pending,
    /// Loading failed; subsequent calls should re-attempt.
    Failed(AssetLoadError),
}

/// Categorised failure reasons surfaced through [`AssetLoadStatus::Failed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetLoadError {
    /// Network or transport-level error while fetching the asset.
    Network(String),
    /// Fetched bytes hashed to a value that didn't match the manifest's expected SHA.
    ShaMismatch {
        /// Asset id (as declared in `BlockInfo::external_assets`).
        id: String,
        /// Expected SHA-256 (hex) recorded in the manifest.
        expected: String,
        /// Actual SHA-256 (hex) computed from the fetched bytes.
        got: String,
    },
    /// No loader is registered for the asset's scheme/source.
    UnknownLoader(String),
    /// Fallback for errors that don't fit the categories above.
    Unknown(String),
}

impl AssetLoadError {
    /// Build a [`Self::ShaMismatch`] without forcing callers to allocate the strings.
    pub fn sha_mismatch(id: &str, expected: &str, got: &str) -> Self {
        Self::ShaMismatch {
            id: id.to_string(),
            expected: expected.to_string(),
            got: got.to_string(),
        }
    }
}

impl fmt::Display for AssetLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(s) => write!(f, "network: {s}"),
            Self::ShaMismatch { id, expected, got } => {
                write!(f, "sha mismatch for {id}: expected {expected}, got {got}")
            }
            Self::UnknownLoader(s) => write!(f, "unknown loader: {s}"),
            Self::Unknown(s) => write!(f, "{s}"),
        }
    }
}

/// Host-side callback invoked from inside `__wafer_host_load_asset`.
/// Implementations are expected to memoise — the first call performs the
/// work, subsequent calls for the same id return `Ready` without re-fetching.
///
/// On native targets the future is `Send` (matches the rest of the runtime).
/// On wasm32 the future is `!Send` so impls can use single-threaded
/// browser primitives like `JsFuture` (the SW host in the browser build does
/// this). The supertrait bound uses `wafer_block::compat::{MaybeSend,
/// MaybeSync}`, matching the pattern already applied to `Block`.
#[wafer_async_trait]
pub trait LoadAssetCallback: wafer_block::MaybeSend + wafer_block::MaybeSync {
    /// Fetch (or look up) the asset identified by `asset_id` and report its current state.
    async fn load(&self, asset_id: &str) -> AssetLoadStatus;
}

/// Default loader for hosts that haven't wired one up. Always returns
/// `Failed(UnknownLoader)` so wasm callers see a clear error rather than
/// hanging.
pub struct NoopAssetLoader;

#[wafer_async_trait]
impl LoadAssetCallback for NoopAssetLoader {
    async fn load(&self, asset_id: &str) -> AssetLoadStatus {
        AssetLoadStatus::Failed(AssetLoadError::UnknownLoader(format!(
            "no asset loader registered (asset={asset_id})"
        )))
    }
}
