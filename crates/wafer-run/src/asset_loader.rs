//! Pluggable async loader for external WASM/JS assets declared via
//! `BlockInfo::external_assets`. The wasm import `__wafer_host_load_asset`
//! suspends the guest and resumes it once the host's `LoadAssetCallback`
//! returns.
//!
//! See `docs/superpowers/specs/2026-04-18-gizza-ai-design.md` §
//! "External asset loading (host side)".

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetLoadStatus {
    /// Asset is loaded and ready to be invoked.
    Ready,
    /// Loading has been requested but is not finished.
    Pending,
    /// Loading failed; subsequent calls should re-attempt.
    Failed(AssetLoadError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetLoadError {
    Network(String),
    ShaMismatch {
        id: String,
        expected: String,
        got: String,
    },
    UnknownLoader(String),
    Unknown(String),
}

impl AssetLoadError {
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
/// browser primitives like `JsFuture` (the SW host in solobase-web does
/// this).
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait LoadAssetCallback: MaybeSendSync {
    async fn load(&self, asset_id: &str) -> AssetLoadStatus;
}

/// Send + Sync on native, no bound on wasm32. Mirrors the conditional bound
/// applied to `Block` so the trait object stored in `Wafer::asset_loader`
/// keeps `Arc<dyn LoadAssetCallback>` thread-safe on native while letting
/// wasm32 callbacks hold `!Send` JS handles.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSendSync: Send + Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + Sync + ?Sized> MaybeSendSync for T {}
#[cfg(target_arch = "wasm32")]
pub trait MaybeSendSync {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSendSync for T {}

/// Default loader for hosts that haven't wired one up. Always returns
/// `Failed(UnknownLoader)` so wasm callers see a clear error rather than
/// hanging.
pub struct NoopAssetLoader;

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl LoadAssetCallback for NoopAssetLoader {
    async fn load(&self, asset_id: &str) -> AssetLoadStatus {
        AssetLoadStatus::Failed(AssetLoadError::UnknownLoader(format!(
            "no asset loader registered (asset={asset_id})"
        )))
    }
}
