//! ImageService trait + shared data types. Mirrors the layout of
//! `interfaces::llm::service` but with image-shaped request/response
//! types.

use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

// ---------- Request side ----------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ImageRequest {
    pub backend_id: String,
    pub model: String,
    pub prompt: String,
    #[serde(default)]
    pub params: ImageParams,
    #[serde(default)]
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ImageParams {
    pub negative_prompt: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub steps: Option<u32>,
    pub guidance_scale: Option<f32>,
    pub seed: Option<u64>,
}

impl ImageRequest {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ImageResponse {
    pub images: Vec<GeneratedImage>,
}

impl ImageResponse {
    pub fn new(images: Vec<GeneratedImage>) -> Self {
        Self { images }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct GeneratedImage {
    pub bytes: Vec<u8>,
    pub mime_type: String,
}

impl GeneratedImage {
    pub fn new(bytes: Vec<u8>, mime_type: impl Into<String>) -> Self {
        Self {
            bytes,
            mime_type: mime_type.into(),
        }
    }
}

// ---------- Model management ----------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ModelInfo {
    pub backend_id: String,
    pub model_id: String,
    pub display_name: String,
    #[serde(default)]
    pub capabilities: ModelCapabilities,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelCapabilities {
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub supports_negative_prompt: bool,
    pub max_steps: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ModelStatus {
    pub state: ModelState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModelState {
    Ready,
    Loading,
    Unloaded,
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct LoadProgress {
    pub stage: String,
    pub bytes_downloaded: Option<u64>,
    pub bytes_total: Option<u64>,
}

impl ModelStatus {
    pub fn ready() -> Self {
        Self {
            state: ModelState::Ready,
            progress: None,
        }
    }
    pub fn loading(progress: f32) -> Self {
        Self {
            state: ModelState::Loading,
            progress: Some(progress),
        }
    }
    pub fn unloaded() -> Self {
        Self {
            state: ModelState::Unloaded,
            progress: None,
        }
    }
}

impl ModelInfo {
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

    pub fn with_capabilities(mut self, capabilities: ModelCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
}

impl LoadProgress {
    pub fn new(stage: impl Into<String>) -> Self {
        Self {
            stage: stage.into(),
            bytes_downloaded: None,
            bytes_total: None,
        }
    }
}

// ---------- Error ----------

#[derive(Debug, Error, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImageError {
    #[error("not supported by this backend")]
    NotSupported,
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("backend error: {0}")]
    BackendError(String),
    #[error("model not found: {0}")]
    ModelNotFound(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("cancelled")]
    Cancelled,
}

// ---------- Trait (placeholder — extended in task 1.3) ----------

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait ImageService: wafer_block::MaybeSend + wafer_block::MaybeSync + 'static {
    async fn generate(
        &self,
        req: ImageRequest,
        cancel: CancellationToken,
    ) -> Result<ImageResponse, ImageError>;

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ImageError>;

    async fn status(&self, backend_id: &str, model_id: &str) -> Result<ModelStatus, ImageError>;

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

    async fn unload_model(&self, _backend_id: &str, _model_id: &str) -> Result<(), ImageError> {
        Err(ImageError::NotSupported)
    }

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
        let mut caps = ModelCapabilities::default();
        caps.max_width = Some(512);
        caps.supports_negative_prompt = true;

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
