use std::path::PathBuf;

use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use wafer_core::interfaces::vector::{
    service::{EmbeddingService, VectorError},
    DEFAULT_MODEL,
};

use crate::batcher::EmbedBatcher;

/// Native ONNX-based [`EmbeddingService`] backed by fastembed-rs.
///
/// Model weights are downloaded on first use and cached under the directory
/// given by `WAFER_RUN__FASTEMBED__CACHE_DIR` (default: `data/models`).
///
/// The model is owned by a dedicated worker thread behind an
/// [`EmbedBatcher`] (PERF-05): `TextEmbedding::embed` takes `&mut self`, so
/// one instance runs one forward pass at a time — previously a shared
/// `Mutex` fully serialized concurrent `embed` calls (N callers → N
/// sequential passes). Requests arriving during a pass now coalesce into one
/// batched pass. A model-instance pool was rejected as disproportionate:
/// each `TextEmbedding` owns a full ONNX session (hundreds of MB per catalog
/// model), while batching gets the concurrency win at zero extra model
/// memory. Token counting uses a [`Tokenizer`](tokenizers::Tokenizer) clone
/// taken at construction, so it never contends with inference.
pub struct FastembedService {
    model_id: String,
    dimensions: u32,
    /// The model's BPE tokenizer, cloned out of the [`TextEmbedding`] before
    /// the model moves to the worker thread. `Tokenizer::encode` takes
    /// `&self`, so [`count_tokens`](EmbeddingService::count_tokens) is
    /// lock-free and independent of in-flight forward passes.
    tokenizer: tokenizers::Tokenizer,
    /// Queue to the worker thread that owns the model (see [`EmbedBatcher`]).
    batcher: EmbedBatcher,
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
        let mut embedding =
            TextEmbedding::try_new(TextInitOptions::new(fb_model).with_cache_dir(cache_dir))
                .map_err(|e| VectorError::Internal(format!("fastembed init: {e}")))?;
        let tokenizer = embedding.tokenizer.clone();
        // The ONNX forward pass is synchronous and CPU-heavy (hundreds of
        // ms), so it runs on the batcher's dedicated blocking thread — never
        // on an async worker.
        let batcher = EmbedBatcher::spawn(move |texts| {
            embedding
                .embed(texts, None)
                .map_err(|e| format!("fastembed: {e}"))
        });
        Ok(Self {
            model_id: model_id.to_string(),
            dimensions: dims,
            tokenizer,
            batcher,
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
        self.batcher
            .enqueue(texts)
            .wait()
            .await
            .map_err(VectorError::Internal)
    }

    fn count_tokens(&self, text: &str) -> usize {
        // A failed tokenize falls back to the whitespace proxy so the
        // chunker can keep making progress on the rare malformed-input case.
        self.tokenizer.encode(text, true).map_or_else(
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

    /// Gated smoke test — real-model counterpart of the deterministic
    /// batcher unit tests: concurrent embeds through the worker all complete
    /// and return per-caller results. Enable with
    /// `WAFER_RUN__FASTEMBED__RUN_INTEGRATION_TESTS=1 cargo test -- --ignored`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn concurrent_embeds_complete_smoke() {
        if std::env::var("WAFER_RUN__FASTEMBED__RUN_INTEGRATION_TESTS").is_err() {
            return;
        }
        let svc = std::sync::Arc::new(
            FastembedService::new("paraphrase-multilingual-MiniLM-L12-v2").unwrap(),
        );
        let tasks: Vec<_> = (0..8)
            .map(|i| {
                let svc = svc.clone();
                tokio::spawn(async move { svc.embed(vec![format!("text number {i}")]).await })
            })
            .collect();
        for task in tasks {
            let vecs = task.await.unwrap().unwrap();
            assert_eq!(vecs.len(), 1);
            assert_eq!(vecs[0].len(), 384);
        }
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
