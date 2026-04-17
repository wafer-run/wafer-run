use std::path::PathBuf;
use std::sync::Mutex;

use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use wafer_core::interfaces::vector::{
    service::{EmbeddingService, VectorError},
    DEFAULT_MODEL,
};

/// Native ONNX-based [`EmbeddingService`] backed by fastembed-rs.
///
/// Model weights are downloaded on first use and cached under the directory
/// given by `SOLOBASE_FASTEMBED_CACHE_DIR` (default: `data/models`).
pub struct FastembedService {
    model_id: String,
    dimensions: u32,
    inner: Mutex<TextEmbedding>,
}

impl FastembedService {
    /// Load the model identified by `model_id` from the catalog.
    ///
    /// Recognized ids (see [`wafer_core::interfaces::vector::catalog`]):
    /// - `bge-m3` (1024 dims)
    /// - `multilingual-e5-small` (384 dims)
    /// - `paraphrase-multilingual-MiniLM-L12-v2` (384 dims)
    pub fn new(model_id: &str) -> Result<Self, VectorError> {
        let (fb_model, dims) = match model_id {
            "bge-m3" => (EmbeddingModel::BGEM3, 1024u32),
            "multilingual-e5-small" => (EmbeddingModel::MultilingualE5Small, 384u32),
            "paraphrase-multilingual-MiniLM-L12-v2" => {
                (EmbeddingModel::ParaphraseMLMiniLML12V2, 384u32)
            }
            other => return Err(VectorError::UnknownModel(other.to_string())),
        };
        let cache_dir = std::env::var("SOLOBASE_FASTEMBED_CACHE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data/models"));
        let embedding =
            TextEmbedding::try_new(TextInitOptions::new(fb_model).with_cache_dir(cache_dir))
                .map_err(|e| VectorError::Internal(format!("fastembed init: {e}")))?;
        Ok(Self {
            model_id: model_id.to_string(),
            dimensions: dims,
            inner: Mutex::new(embedding),
        })
    }

    /// Convenience constructor for the catalog's default model.
    pub fn default_model() -> Result<Self, VectorError> {
        Self::new(DEFAULT_MODEL)
    }
}

#[async_trait::async_trait]
impl EmbeddingService for FastembedService {
    fn model(&self) -> &str {
        &self.model_id
    }

    fn dimensions(&self) -> u32 {
        self.dimensions
    }

    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, VectorError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| VectorError::Internal(format!("fastembed mutex poisoned: {e}")))?;
        guard
            .embed(texts, None)
            .map_err(|e| VectorError::Internal(format!("fastembed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wafer_core::interfaces::vector::service::EmbeddingService;

    /// Gated smoke test — first run downloads ~120 MB. Enable with
    /// `SOLOBASE_RUN_FASTEMBED_TESTS=1 cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn embed_paraphrase_minilm_smoke() {
        if std::env::var("SOLOBASE_RUN_FASTEMBED_TESTS").is_err() {
            return;
        }
        let svc = FastembedService::new("paraphrase-multilingual-MiniLM-L12-v2").unwrap();
        assert_eq!(svc.dimensions(), 384);
        let vecs = svc
            .embed(vec!["hello world".into(), "goodbye".into()])
            .await
            .unwrap();
        assert_eq!(vecs.len(), 2);
        assert_eq!(vecs[0].len(), 384);
    }

    #[tokio::test]
    async fn unknown_model_errors() {
        match FastembedService::new("nope-not-real") {
            Err(VectorError::UnknownModel(name)) => assert_eq!(name, "nope-not-real"),
            Err(other) => panic!("expected UnknownModel, got {other:?}"),
            Ok(_) => panic!("expected UnknownModel error, got Ok"),
        }
    }
}
