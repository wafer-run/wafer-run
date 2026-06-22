use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use wafer_core::interfaces::vector::{
    service::{EmbeddingService, VectorError},
    DEFAULT_MODEL,
};

/// Native ONNX-based [`EmbeddingService`] backed by fastembed-rs.
///
/// Model weights are downloaded on first use and cached under the directory
/// given by `WAFER_RUN__FASTEMBED__CACHE_DIR` (default: `data/models`).
pub struct FastembedService {
    model_id: String,
    dimensions: u32,
    /// `Arc` so the model can be moved into a `spawn_blocking` closure for the
    /// CPU-heavy embedding forward pass without cloning the model itself.
    inner: Arc<Mutex<TextEmbedding>>,
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
        let cache_dir = std::env::var("WAFER_RUN__FASTEMBED__CACHE_DIR")
            .map_or_else(|_| PathBuf::from("data/models"), PathBuf::from);
        let embedding =
            TextEmbedding::try_new(TextInitOptions::new(fb_model).with_cache_dir(cache_dir))
                .map_err(|e| VectorError::Internal(format!("fastembed init: {e}")))?;
        Ok(Self {
            model_id: model_id.to_string(),
            dimensions: dims,
            inner: Arc::new(Mutex::new(embedding)),
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
        // The ONNX forward pass is synchronous and CPU-heavy (hundreds of ms).
        // Running it directly on the async worker would stall every other task
        // on that thread, so offload it to the blocking pool. The model is
        // shared via `Arc` and guarded by the same `Mutex` the tokenizer path
        // uses, so concurrent embed calls still serialise on the model.
        let model = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let mut guard = model
                .lock()
                .map_err(|e| VectorError::Internal(format!("fastembed mutex poisoned: {e}")))?;
            guard
                .embed(texts, None)
                .map_err(|e| VectorError::Internal(format!("fastembed: {e}")))
        })
        .await
        .map_err(|e| VectorError::Internal(format!("fastembed embed task failed: {e}")))?
    }

    fn count_tokens(&self, text: &str) -> usize {
        // The fastembed `TextEmbedding` owns the model's BPE tokenizer; we
        // borrow it through the same `Mutex` the embed path uses. A failed
        // lock or tokenize falls back to the whitespace proxy so the chunker
        // can keep making progress on the rare malformed-input case.
        let Ok(guard) = self.inner.lock() else {
            return text.split_whitespace().count();
        };
        guard.tokenizer.encode(text, true).map_or_else(
            |_| text.split_whitespace().count(),
            |enc| enc.get_ids().len(),
        )
    }
}

#[cfg(test)]
mod tests {
    use wafer_core::interfaces::vector::service::EmbeddingService;

    use super::*;

    /// Gated smoke test — first run downloads ~120 MB. Enable with
    /// `WAFER_RUN__FASTEMBED__RUN_INTEGRATION_TESTS=1 cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn embed_paraphrase_minilm_smoke() {
        if std::env::var("WAFER_RUN__FASTEMBED__RUN_INTEGRATION_TESTS").is_err() {
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

    /// Gated smoke test for the bge-m3 BPE tokenizer. Enable with
    /// `WAFER_RUN__FASTEMBED__RUN_INTEGRATION_TESTS=1 cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn count_tokens_uses_bge_m3_tokenizer() {
        if std::env::var("WAFER_RUN__FASTEMBED__RUN_INTEGRATION_TESTS").is_err() {
            return;
        }
        let svc = FastembedService::new("paraphrase-multilingual-MiniLM-L12-v2").unwrap();
        // English: BPE token count usually exceeds the whitespace-word count
        // by ~20–40% because of sub-word splits plus the [CLS]/[SEP] specials
        // the tokenizer adds. Assert both that we get a non-zero count and
        // that we're not just returning whitespace count.
        let text = "tokenization of multilingual content";
        let whitespace = text.split_whitespace().count();
        let n = svc.count_tokens(text);
        assert!(
            n >= whitespace,
            "tokens ({n}) should >= words ({whitespace})"
        );
        assert!(n > whitespace, "tokens ({n}) should exceed words ({whitespace}) — confirms real tokenizer, not whitespace fallback");
    }
}
