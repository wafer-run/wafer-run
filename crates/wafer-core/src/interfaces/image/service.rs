//! ImageService trait + shared data types. Mirrors the layout of
//! `interfaces::llm::service` but with image-shaped request/response
//! types.

use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use wafer_block_macro::wafer_async_trait;

// `ModelStatus`, `ModelState`, and `LoadProgress` are field-identical with the
// LLM interface, so they live in `interfaces::model_common` and are re-used
// here. The capability-specific `ModelInfo` / `ModelCapabilities` below stay
// image-specialized.
pub use crate::interfaces::model_common::{LoadProgress, ModelState, ModelStatus};

// ---------- Request side ----------

/// A text-to-image generation request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ImageRequest {
    /// Identifier of the backend that should serve this request.
    pub backend_id: String,
    /// Model id within the chosen backend.
    pub model: String,
    /// Positive prompt describing the desired image.
    pub prompt: String,
    /// Common generation parameters.
    #[serde(default)]
    pub params: ImageParams,
    /// Backend-specific extra arguments passed through verbatim.
    #[serde(default)]
    pub extra: serde_json::Value,
}

/// Common knobs supported by most diffusion / image backends.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ImageParams {
    /// Optional negative prompt to steer the model away from concepts.
    pub negative_prompt: Option<String>,
    /// Output width in pixels.
    pub width: Option<u32>,
    /// Output height in pixels.
    pub height: Option<u32>,
    /// Number of diffusion / sampling steps.
    pub steps: Option<u32>,
    /// Classifier-free guidance scale.
    pub guidance_scale: Option<f32>,
    /// Deterministic seed for reproducible output.
    pub seed: Option<u64>,
}

impl ImageRequest {
    /// Build a request with the given `backend_id`, `model`, and `prompt` and
    /// default parameters.
    pub fn new(
        backend_id: impl Into<String>,
        model: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Self {
        Self {
            backend_id: backend_id.into(),
            model: model.into(),
            prompt: prompt.into(),
            params: ImageParams::default(),
            extra: serde_json::Value::Null,
        }
    }
}

// ---------- Response side ----------

/// Response from a successful [`ImageService::generate`] call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ImageResponse {
    /// One or more generated images.
    pub images: Vec<GeneratedImage>,
}

impl ImageResponse {
    /// Wrap a vector of `GeneratedImage` as a response.
    pub fn new(images: Vec<GeneratedImage>) -> Self {
        Self { images }
    }
}

/// A single image returned by the backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct GeneratedImage {
    /// Encoded image bytes (format determined by `mime_type`).
    pub bytes: Vec<u8>,
    /// MIME type of `bytes` (e.g. `image/png`).
    pub mime_type: String,
}

impl GeneratedImage {
    /// Build a `GeneratedImage` from raw `bytes` and a `mime_type`.
    pub fn new(bytes: Vec<u8>, mime_type: impl Into<String>) -> Self {
        Self {
            bytes,
            mime_type: mime_type.into(),
        }
    }
}

// ---------- Model management ----------

/// Metadata describing a model exposed by an image backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ModelInfo {
    /// Backend that owns this model.
    pub backend_id: String,
    /// Model id within the backend.
    pub model_id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Optional capability hints (size limits, supported knobs).
    #[serde(default)]
    pub capabilities: ModelCapabilities,
}

/// Optional capability hints attached to a [`ModelInfo`].
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelCapabilities {
    /// Maximum output width supported, if known.
    pub max_width: Option<u32>,
    /// Maximum output height supported, if known.
    pub max_height: Option<u32>,
    /// Whether the model honors `negative_prompt`.
    pub supports_negative_prompt: bool,
    /// Maximum diffusion / sampling steps, if known.
    pub max_steps: Option<u32>,
}

impl ModelInfo {
    /// Build a `ModelInfo` with default capabilities.
    pub fn new(
        backend_id: impl Into<String>,
        model_id: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Self {
        Self {
            backend_id: backend_id.into(),
            model_id: model_id.into(),
            display_name: display_name.into(),
            capabilities: ModelCapabilities::default(),
        }
    }

    /// Replace the `capabilities` field; chainable on `new()`.
    pub fn with_capabilities(mut self, capabilities: ModelCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
}

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
    fn image_request_roundtrip() {
        let req = ImageRequest {
            backend_id: "transformers-image".into(),
            model: "Xenova/sd-turbo".into(),
            prompt: "a cat".into(),
            params: ImageParams {
                steps: Some(1),
                width: Some(512),
                height: Some(512),
                ..Default::default()
            },
            extra: serde_json::Value::Null,
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: ImageRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn image_response_roundtrip() {
        let resp = ImageResponse {
            images: vec![GeneratedImage {
                bytes: b"\x89PNG\r\n\x1a\n".to_vec(),
                mime_type: "image/png".into(),
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: ImageResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, resp);
    }

    #[test]
    fn image_response_new_and_generated_image_new() {
        let img = GeneratedImage::new(b"\x89PNG\r\n\x1a\n".to_vec(), "image/png");
        assert_eq!(img.bytes.len(), 8);
        assert_eq!(img.mime_type, "image/png");

        let resp = ImageResponse::new(vec![img.clone()]);
        assert_eq!(resp.images, vec![img]);
    }

    #[test]
    fn model_info_with_capabilities_chaining() {
        let caps = ModelCapabilities {
            max_width: Some(512),
            supports_negative_prompt: true,
            ..Default::default()
        };

        let info = ModelInfo::new("transformers-image", "Xenova/sd-turbo", "SD-Turbo")
            .with_capabilities(caps);

        assert_eq!(info.capabilities.max_width, Some(512));
        assert!(info.capabilities.supports_negative_prompt);
        assert_eq!(info.model_id, "Xenova/sd-turbo");
    }

    #[test]
    fn image_error_display_and_roundtrip() {
        let err = ImageError::InvalidRequest("missing prompt".into());
        assert_eq!(err.to_string(), "invalid request: missing prompt");
        let json = serde_json::to_string(&err).unwrap();
        let decoded: ImageError = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, err);
    }

    #[test]
    fn model_info_roundtrip() {
        let info = ModelInfo {
            backend_id: "transformers-image".into(),
            model_id: "Xenova/sd-turbo".into(),
            display_name: "SD-Turbo".into(),
            capabilities: ModelCapabilities {
                max_width: Some(512),
                max_height: Some(512),
                supports_negative_prompt: true,
                max_steps: Some(4),
            },
        };
        let json = serde_json::to_string(&info).unwrap();
        let decoded: ModelInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, info);
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
