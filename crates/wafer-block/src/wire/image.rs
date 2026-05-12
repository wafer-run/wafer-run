//! Wire-format types for the image service.
//!
//! Mirrors `interfaces::image::service` in wafer-core. Field-identical
//! types — the handler in `wafer-core` converts between wire and service
//! representations because wafer-block is a leaf crate that cannot
//! depend on wafer-core.

use serde::{Deserialize, Serialize};

// ---- Request ----

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
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
pub struct ImageParams {
    pub negative_prompt: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub steps: Option<u32>,
    pub guidance_scale: Option<f32>,
    pub seed: Option<u64>,
}

// ---- Response ----

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ImageResponse {
    #[serde(default)]
    pub images: Vec<GeneratedImage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GeneratedImage {
    pub bytes: Vec<u8>,
    pub mime_type: String,
}

// ---- Model management ----

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StatusRequest {
    pub backend_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LoadModelRequest {
    pub backend_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UnloadModelRequest {
    pub backend_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelInfo {
    pub backend_id: String,
    pub model_id: String,
    pub display_name: String,
    #[serde(default)]
    pub capabilities: ModelCapabilities,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub supports_negative_prompt: bool,
    pub max_steps: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelStatus {
    pub state: ModelState,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ModelState {
    Ready,
    Loading,
    Unloaded,
    Error { message: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoadProgress {
    pub stage: String,
    pub bytes_downloaded: Option<u64>,
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
