//! Model-management types shared by the LLM and image interfaces.
//!
//! `ModelStatus`, `ModelState`, and `LoadProgress` are field-identical across
//! both capabilities, so they live here and are re-used from
//! `interfaces::llm::service` and `interfaces::image::service`. The
//! capability-specific structs (`ModelInfo` / `ModelCapabilities`) stay
//! specialized in each interface because their shapes differ (LLM tracks
//! streaming / tools / vision; image tracks max dimensions / steps).

use serde::{Deserialize, Serialize};

/// Current lifecycle status of a model on a backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ModelStatus {
    /// High-level state.
    pub state: ModelState,
    /// 0.0–1.0 when `state == Loading`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
}

/// High-level lifecycle state of a backend model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModelState {
    /// Local: weights loaded. Remote: endpoint reachable.
    Ready,
    /// Local: weights currently downloading or initializing.
    Loading,
    /// Local only — weights not in memory.
    Unloaded,
    /// Model is in a failed state.
    Error {
        /// Failure message.
        message: String,
    },
}

/// Streaming progress event emitted while a backend loads a model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct LoadProgress {
    /// e.g. `"downloading"`, `"initializing"`, `"compiling"`.
    pub stage: String,
    /// Bytes downloaded so far, if known.
    pub bytes_downloaded: Option<u64>,
    /// Total bytes to download, if known.
    pub bytes_total: Option<u64>,
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
    fn model_status_roundtrip() {
        let s = ModelStatus::loading(0.25);
        let json = serde_json::to_string(&s).unwrap();
        let decoded: ModelStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, s);
    }
}
