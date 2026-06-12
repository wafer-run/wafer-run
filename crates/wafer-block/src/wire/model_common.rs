//! Model-management wire types shared by the LLM and image services.
//!
//! `StatusRequest`, `LoadModelRequest`, `UnloadModelRequest`, `ModelStatus`,
//! `ModelState`, and `LoadProgress` are field-identical across both
//! capabilities, so they are defined once here and re-exported from
//! [`super::llm`] and [`super::image`] (existing import paths keep working).
//! The capability-specific `ModelInfo` / `ModelCapabilities` structs stay
//! specialized in each module because their shapes differ (LLM tracks
//! streaming / tools / vision; image tracks max dimensions / steps).

use serde::{Deserialize, Serialize};

/// Request for `llm.status` / `image.status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusRequest {
    /// Backend identifier.
    pub backend_id: String,
    /// Model identifier within the backend.
    pub model_id: String,
}

/// Request for `llm.load_model` / `image.load_model`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoadModelRequest {
    /// Backend identifier.
    pub backend_id: String,
    /// Model identifier to load.
    pub model_id: String,
}

/// Request for `llm.unload_model` / `image.unload_model`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnloadModelRequest {
    /// Backend identifier.
    pub backend_id: String,
    /// Model identifier to unload.
    pub model_id: String,
}

/// Current lifecycle status of a model on a backend, returned by
/// `llm.status` / `image.status`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelStatus {
    /// High-level state.
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

impl ModelStatus {
    /// A status reporting the model is ready to serve.
    pub fn ready() -> Self {
        Self {
            state: ModelState::Ready,
            progress: None,
        }
    }

    /// A status reporting the model is currently loading at the given progress fraction.
    pub fn loading(progress: f32) -> Self {
        Self {
            state: ModelState::Loading,
            progress: Some(progress),
        }
    }

    /// A status reporting the model is known but not currently resident.
    pub fn unloaded() -> Self {
        Self {
            state: ModelState::Unloaded,
            progress: None,
        }
    }

    /// An errored status carrying the failure message. Progress is cleared.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            state: ModelState::Error {
                message: message.into(),
            },
            progress: None,
        }
    }
}

/// High-level lifecycle state of a model on a backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ModelState {
    /// Local: weights loaded. Remote: endpoint reachable.
    Ready,
    /// Local: weights currently downloading or initializing.
    Loading,
    /// Local only — weights not in memory.
    Unloaded,
    /// Loading or serving failed.
    Error {
        /// Failure message.
        message: String,
    },
}

/// Streaming progress event emitted while a backend loads a model.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoadProgress {
    /// Free-form stage label (e.g. `"downloading"`, `"initializing"`, `"compiling"`).
    pub stage: String,
    /// Bytes downloaded so far, if known.
    pub bytes_downloaded: Option<u64>,
    /// Total bytes to download, if known.
    pub bytes_total: Option<u64>,
}

impl LoadProgress {
    /// Minimal constructor. Byte counters default to `None`; callers set them
    /// via field access when the backend reports them.
    pub fn new(stage: impl Into<String>) -> Self {
        Self {
            stage: stage.into(),
            bytes_downloaded: None,
            bytes_total: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    #[test]
    fn status_request_round_trips() {
        let original = StatusRequest {
            backend_id: "b1".into(),
            model_id: "m1".into(),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: StatusRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn model_status_constructors_set_expected_state() {
        assert_eq!(ModelStatus::ready().state, ModelState::Ready);
        assert!(matches!(
            ModelStatus::loading(0.5),
            ModelStatus {
                state: ModelState::Loading,
                progress: Some(p)
            } if (p - 0.5).abs() < f32::EPSILON
        ));
        assert_eq!(ModelStatus::unloaded().state, ModelState::Unloaded);
        assert!(matches!(
            ModelStatus::error("boom").state,
            ModelState::Error { message } if message == "boom"
        ));
    }

    #[test]
    fn load_progress_new_defaults_byte_counts() {
        let p = LoadProgress::new("downloading");
        assert_eq!(p.stage, "downloading");
        assert!(p.bytes_downloaded.is_none());
        assert!(p.bytes_total.is_none());
    }

    #[test]
    fn model_status_round_trips() {
        let original = ModelStatus::loading(0.25);
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ModelStatus = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded, original);
    }

    // ----- Schema locks -----

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

    #[test]
    fn schema_lock_load_model_request() {
        let req = LoadModelRequest::default();
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82aa6261636b656e645f6964a0a86d6f64656c5f6964a0",
            "LoadModelRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_unload_model_request() {
        let req = UnloadModelRequest::default();
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82aa6261636b656e645f6964a0a86d6f64656c5f6964a0",
            "UnloadModelRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_model_status() {
        let s = ModelStatus::ready();
        let encoded = codec::encode(&s).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        // {"state": "Ready"} — `progress` omitted via skip_serializing_if.
        assert_eq!(
            hex, "81a57374617465a55265616479",
            "ModelStatus schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_load_progress() {
        let p = LoadProgress::new("downloading");
        let encoded = codec::encode(&p).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "83a57374616765ab646f776e6c6f6164696e67b062797465735f646f776e6c6f61646564c0ab62797465735f746f74616cc0",
            "LoadProgress schema changed — review consumer impact before updating this literal"
        );
    }
}
