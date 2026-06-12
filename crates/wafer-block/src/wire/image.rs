//! Wire-format types for the image service.
//!
//! These are the canonical image types: `wafer_core::interfaces::image::service`
//! re-exports them so host-side service impls and wire-level consumers share
//! one definition.
//!
//! The model-management types (`StatusRequest`, `LoadModelRequest`,
//! `UnloadModelRequest`, `ModelStatus`, `ModelState`, `LoadProgress`) are
//! shared verbatim with the LLM service and live in [`super::model_common`];
//! they are re-exported here so existing `wire::image::*` import paths keep
//! working.

use serde::{Deserialize, Serialize};

pub use super::model_common::{
    LoadModelRequest, LoadProgress, ModelState, ModelStatus, StatusRequest, UnloadModelRequest,
};

// ---- Request ----

/// Request for `image.generate`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ImageRequest {
    /// Identifier of the configured image backend to dispatch to.
    pub backend_id: String,
    /// Model name within the backend.
    pub model: String,
    /// Positive text prompt describing the desired image.
    pub prompt: String,
    /// Generation parameters.
    #[serde(default)]
    pub params: ImageParams,
    /// Backend-specific pass-through parameters (free-form JSON).
    #[serde(default)]
    pub extra: serde_json::Value,
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

/// Generation parameters for [`ImageRequest`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ImageParams {
    /// Negative prompt — content to avoid (model-dependent).
    pub negative_prompt: Option<String>,
    /// Output image width in pixels.
    pub width: Option<u32>,
    /// Output image height in pixels.
    pub height: Option<u32>,
    /// Number of diffusion / denoising steps.
    pub steps: Option<u32>,
    /// Classifier-free-guidance scale.
    pub guidance_scale: Option<f32>,
    /// PRNG seed for deterministic generation.
    pub seed: Option<u64>,
}

// ---- Response ----

/// Response for `image.generate`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ImageResponse {
    /// Generated images, one per requested sample.
    #[serde(default)]
    pub images: Vec<GeneratedImage>,
}

impl ImageResponse {
    /// Wrap a vector of `GeneratedImage` as a response.
    pub fn new(images: Vec<GeneratedImage>) -> Self {
        Self { images }
    }
}

/// A single generated image returned from the backend.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GeneratedImage {
    /// Raw image bytes.
    pub bytes: Vec<u8>,
    /// MIME type of `bytes`.
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

// ---- Model management ----

/// Descriptor of an available image model returned by `image.list_models`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelInfo {
    /// Backend identifier the model is hosted in.
    pub backend_id: String,
    /// Backend-side model identifier.
    pub model_id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Declared model capabilities.
    #[serde(default)]
    pub capabilities: ModelCapabilities,
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

/// Capability flags for an image [`ModelInfo`].
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCapabilities {
    /// Maximum supported output width in pixels.
    pub max_width: Option<u32>,
    /// Maximum supported output height in pixels.
    pub max_height: Option<u32>,
    /// Whether the model accepts a negative prompt.
    pub supports_negative_prompt: bool,
    /// Maximum supported diffusion-step count.
    pub max_steps: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    #[test]
    fn image_request_round_trips() {
        let original = ImageRequest {
            backend_id: "transformers-image".into(),
            model: "Xenova/sd-turbo".into(),
            prompt: "hi".into(),
            params: ImageParams {
                steps: Some(1),
                width: Some(512),
                height: Some(512),
                ..Default::default()
            },
            extra: serde_json::Value::Null,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ImageRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.prompt, "hi");
        assert_eq!(decoded.params.steps, Some(1));
    }

    #[test]
    fn image_response_round_trips() {
        let original = ImageResponse {
            images: vec![GeneratedImage {
                bytes: vec![1, 2, 3, 4],
                mime_type: "image/png".into(),
            }],
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ImageResponse = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded, original);
        assert_eq!(decoded.images[0].bytes, vec![1, 2, 3, 4]);
    }

    #[test]
    fn image_constructors_set_expected_fields() {
        let img = GeneratedImage::new(vec![1, 2, 3], "image/png");
        assert_eq!(img.bytes, vec![1, 2, 3]);
        assert_eq!(img.mime_type, "image/png");

        let resp = ImageResponse::new(vec![img.clone()]);
        assert_eq!(resp.images, vec![img]);

        let req = ImageRequest::new("transformers-image", "Xenova/sd-turbo", "a cat");
        assert_eq!(req.prompt, "a cat");
        assert_eq!(req.params, ImageParams::default());
        assert_eq!(req.extra, serde_json::Value::Null);

        let info = ModelInfo::new("transformers-image", "Xenova/sd-turbo", "SD-Turbo")
            .with_capabilities(ModelCapabilities {
                max_width: Some(512),
                supports_negative_prompt: true,
                ..Default::default()
            });
        assert_eq!(info.capabilities.max_width, Some(512));
        assert!(info.capabilities.supports_negative_prompt);
    }
}
