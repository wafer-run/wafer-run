//! Wire-format types for the image service.
//!
//! Mirrors `interfaces::image::service` in wafer-core. Field-identical
//! types — the handler in `wafer-core` converts between wire and service
//! representations because wafer-block is a leaf crate that cannot
//! depend on wafer-core.

use serde::{Deserialize, Serialize};

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

/// A single generated image returned from the backend.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GeneratedImage {
    /// Raw image bytes.
    pub bytes: Vec<u8>,
    /// MIME type of `bytes`.
    pub mime_type: String,
}

// ---- Model management ----

/// Request for `image.status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StatusRequest {
    /// Backend identifier.
    pub backend_id: String,
    /// Model identifier within the backend.
    pub model_id: String,
}

/// Request for `image.load_model`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LoadModelRequest {
    /// Backend identifier.
    pub backend_id: String,
    /// Model identifier to load.
    pub model_id: String,
}

/// Request for `image.unload_model`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UnloadModelRequest {
    /// Backend identifier.
    pub backend_id: String,
    /// Model identifier to unload.
    pub model_id: String,
}

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

/// Loaded-model status returned by `image.status`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelStatus {
    /// Current model state.
    pub state: ModelState,
    /// Optional 0.0..1.0 progress when `state == Loading`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
}

impl Default for ModelStatus {
    fn default() -> Self {
        Self {
            state: ModelState::Unloaded,
            progress: None,
        }
    }
}

/// Lifecycle state of an image model on a backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ModelState {
    /// Model is loaded and ready to serve requests.
    Ready,
    /// Model is currently being loaded.
    Loading,
    /// Model is known to the backend but not loaded.
    Unloaded,
    /// Loading or serving failed.
    Error {
        /// Error description.
        message: String,
    },
}

/// Progress detail for an image-model load operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoadProgress {
    /// Free-form stage label (e.g. `"downloading"`, `"compiling"`).
    pub stage: String,
    /// Bytes downloaded so far (if known).
    pub bytes_downloaded: Option<u64>,
    /// Expected total bytes (if known).
    pub bytes_total: Option<u64>,
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
        assert_eq!(decoded.images.len(), 1);
        assert_eq!(decoded.images[0].bytes, vec![1, 2, 3, 4]);
    }

    #[test]
    fn schema_lock_status_request() {
        let req = StatusRequest::default();
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82aa6261636b656e645f6964a0a86d6f64656c5f6964a0",
            "StatusRequest schema changed — review consumer impact before updating this literal"
        );
    }
}
