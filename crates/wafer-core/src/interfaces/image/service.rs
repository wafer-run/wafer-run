//! ImageService trait + shared data types. Mirrors the layout of
//! `interfaces::llm::service` but with image-shaped request/response
//! types.
//!
//! The data types (`ImageRequest`, `ImageResponse`, `ModelInfo`, …) are the
//! canonical wire types from `wafer_block::wire::image`, re-exported here so
//! service impls and wire-level consumers share one definition — there is no
//! separate service-side representation and no conversion layer. Only the
//! genuinely service-side items (`ImageService`, `ImageError`) live in this
//! module.

use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
pub use wafer_block::wire::image::{
    GeneratedImage, ImageParams, ImageRequest, ImageResponse, LoadProgress, ModelCapabilities,
    ModelInfo, ModelState, ModelStatus,
};
use wafer_block_macro::wafer_async_trait;

// ---------- Error ----------

/// Errors returned by [`ImageService`] operations.
#[derive(Debug, Error, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImageError {
    /// Operation is not implemented by this backend.
    #[error("not supported by this backend")]
    NotSupported,
    /// Caller supplied an invalid request.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// Backend reported an internal failure.
    #[error("backend error: {0}")]
    BackendError(String),
    /// Requested model is not known to the backend.
    #[error("model not found: {0}")]
    ModelNotFound(String),
    /// Network-level failure (e.g. upstream provider).
    #[error("network error: {0}")]
    Network(String),
    /// Operation was cancelled via its `CancellationToken`.
    #[error("cancelled")]
    Cancelled,
}

// ---------- Trait (placeholder — extended in task 1.3) ----------

/// Image-generation backend interface.
#[wafer_async_trait]
pub trait ImageService: wafer_block::MaybeSend + wafer_block::MaybeSync + 'static {
    /// Generate one or more images from `req`. Honors `cancel` for early termination.
    async fn generate(
        &self,
        req: ImageRequest,
        cancel: CancellationToken,
    ) -> Result<ImageResponse, ImageError>;

    /// Enumerate models exposed by this backend.
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ImageError>;

    /// Report the current lifecycle status of `model_id` on `backend_id`.
    async fn status(&self, backend_id: &str, model_id: &str) -> Result<ModelStatus, ImageError>;

    /// Begin loading `model_id`, emitting progress events.
    /// Default impl emits a single `NotSupported` error.
    fn load_model(
        &self,
        _backend_id: &str,
        _model_id: &str,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<LoadProgress, ImageError>> {
        Box::pin(futures::stream::once(async {
            Err(ImageError::NotSupported)
        }))
    }

    /// Unload `model_id` if the backend supports doing so.
    /// Default impl returns `NotSupported`.
    async fn unload_model(&self, _backend_id: &str, _model_id: &str) -> Result<(), ImageError> {
        Err(ImageError::NotSupported)
    }

    /// Return `true` if this service handles requests for the given `backend_id`.
    /// Default impl returns `false` (used by the multi-backend router).
    fn claims_backend(&self, _backend_id: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_error_display_and_roundtrip() {
        let err = ImageError::InvalidRequest("missing prompt".into());
        assert_eq!(err.to_string(), "invalid request: missing prompt");
        let json = serde_json::to_string(&err).unwrap();
        let decoded: ImageError = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, err);
    }

    /// Minimal impl that only overrides the required trait methods, to
    /// exercise the default impls for load_model / unload_model / claims_backend.
    struct MinimalImage;

    #[async_trait::async_trait]
    impl ImageService for MinimalImage {
        async fn generate(
            &self,
            _req: ImageRequest,
            _cancel: CancellationToken,
        ) -> Result<ImageResponse, ImageError> {
            Ok(ImageResponse { images: vec![] })
        }

        async fn list_models(&self) -> Result<Vec<ModelInfo>, ImageError> {
            Ok(vec![])
        }

        async fn status(
            &self,
            _backend_id: &str,
            _model_id: &str,
        ) -> Result<ModelStatus, ImageError> {
            Ok(ModelStatus::ready())
        }
    }

    #[tokio::test]
    async fn default_load_model_returns_not_supported() {
        use futures::StreamExt;
        let svc = MinimalImage;
        let mut stream = svc.load_model("x", "y", CancellationToken::new());
        let first = stream.next().await.unwrap();
        assert!(matches!(first, Err(ImageError::NotSupported)));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn default_unload_model_returns_not_supported() {
        let svc = MinimalImage;
        assert!(matches!(
            svc.unload_model("x", "y").await,
            Err(ImageError::NotSupported)
        ));
    }

    #[test]
    fn default_claims_backend_returns_false() {
        let svc = MinimalImage;
        assert!(!svc.claims_backend("anything"));
    }
}
