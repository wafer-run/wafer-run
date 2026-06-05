//! `MultiBackendImageService` — dispatches generate / model-management
//! operations across registered `ImageService` impls keyed by `backend_id`.
//!
//! Mirrors `interfaces::llm::router::MultiBackendLlmService`.

use std::sync::Arc;

use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;
use wafer_block_macro::wafer_async_trait;

use super::service::{
    ImageError, ImageRequest, ImageResponse, ImageService, LoadProgress, ModelInfo, ModelStatus,
};

/// `ImageService` that fans `claims_backend`-routed calls out to a list of
/// registered backend implementations.
pub struct MultiBackendImageService {
    impls: Vec<(String, Arc<dyn ImageService>)>,
}

impl MultiBackendImageService {
    /// Construct an empty router; backends are added with [`register`](Self::register).
    pub fn new() -> Self {
        Self { impls: Vec::new() }
    }

    /// Register an ImageService impl under a label. Earlier impls win when
    /// more than one claims the same `backend_id`.
    pub fn register(
        &mut self,
        label: impl Into<String>,
        service: Arc<dyn ImageService>,
    ) -> &mut Self {
        self.impls.push((label.into(), service));
        self
    }

    fn find(&self, backend_id: &str) -> Option<&Arc<dyn ImageService>> {
        self.impls
            .iter()
            .find(|(_, svc)| svc.claims_backend(backend_id))
            .map(|(_, svc)| svc)
    }
}

impl Default for MultiBackendImageService {
    fn default() -> Self {
        Self::new()
    }
}

#[wafer_async_trait]
impl ImageService for MultiBackendImageService {
    async fn generate(
        &self,
        req: ImageRequest,
        cancel: CancellationToken,
    ) -> Result<ImageResponse, ImageError> {
        match self.find(&req.backend_id) {
            Some(svc) => svc.generate(req, cancel).await,
            None => Err(ImageError::InvalidRequest(format!(
                "unknown backend_id: {}",
                req.backend_id
            ))),
        }
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ImageError> {
        let mut all = Vec::new();
        for (label, svc) in &self.impls {
            match svc.list_models().await {
                Ok(models) => all.extend(models),
                Err(e) => {
                    // Tolerate per-backend failures; one bad backend shouldn't
                    // poison the aggregate. Log so a misconfigured / down
                    // backend is diagnosable rather than silently dropped.
                    tracing::warn!(backend = %label, error = %e, "list_models failed");
                }
            }
        }
        Ok(all)
    }

    async fn status(&self, backend_id: &str, model_id: &str) -> Result<ModelStatus, ImageError> {
        match self.find(backend_id) {
            Some(svc) => svc.status(backend_id, model_id).await,
            None => Err(ImageError::InvalidRequest(format!(
                "unknown backend_id: {backend_id}"
            ))),
        }
    }

    fn load_model(
        &self,
        backend_id: &str,
        model_id: &str,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<LoadProgress, ImageError>> {
        #[expect(
            clippy::option_if_let_else,
            reason = "None arm is multi-statement (owns backend_id into an async stream); match reads clearer than map_or_else"
        )]
        match self.find(backend_id) {
            Some(svc) => svc.load_model(backend_id, model_id, cancel),
            None => {
                let id = backend_id.to_string();
                Box::pin(futures::stream::once(async move {
                    Err(ImageError::InvalidRequest(format!(
                        "unknown backend_id: {id}"
                    )))
                }))
            }
        }
    }

    async fn unload_model(&self, backend_id: &str, model_id: &str) -> Result<(), ImageError> {
        match self.find(backend_id) {
            Some(svc) => svc.unload_model(backend_id, model_id).await,
            None => Err(ImageError::InvalidRequest(format!(
                "unknown backend_id: {backend_id}"
            ))),
        }
    }

    fn claims_backend(&self, backend_id: &str) -> bool {
        self.impls
            .iter()
            .any(|(_, svc)| svc.claims_backend(backend_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interfaces::image::service::{GeneratedImage, ImageParams};

    struct Stub {
        backend_id: &'static str,
        models: Vec<ModelInfo>,
    }

    #[async_trait::async_trait]
    impl ImageService for Stub {
        async fn generate(
            &self,
            req: ImageRequest,
            _cancel: CancellationToken,
        ) -> Result<ImageResponse, ImageError> {
            Ok(ImageResponse {
                images: vec![GeneratedImage {
                    bytes: req.prompt.into_bytes(),
                    mime_type: format!("image/{}", self.backend_id),
                }],
            })
        }

        async fn list_models(&self) -> Result<Vec<ModelInfo>, ImageError> {
            Ok(self.models.clone())
        }

        async fn status(
            &self,
            _backend_id: &str,
            _model_id: &str,
        ) -> Result<ModelStatus, ImageError> {
            Ok(ModelStatus::ready())
        }

        fn claims_backend(&self, backend_id: &str) -> bool {
            backend_id == self.backend_id
        }
    }

    #[tokio::test]
    async fn generate_dispatches_to_claiming_backend() {
        let mut router = MultiBackendImageService::new();
        router.register(
            "a",
            Arc::new(Stub {
                backend_id: "alpha",
                models: vec![],
            }),
        );
        router.register(
            "b",
            Arc::new(Stub {
                backend_id: "bravo",
                models: vec![],
            }),
        );

        let req = ImageRequest {
            backend_id: "bravo".into(),
            model: "m".into(),
            prompt: "hi".into(),
            params: ImageParams::default(),
            extra: serde_json::Value::Null,
        };
        let resp = router
            .generate(req, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(resp.images[0].mime_type, "image/bravo");
    }

    #[tokio::test]
    async fn generate_unknown_backend_returns_invalid_request() {
        let router = MultiBackendImageService::new();
        let err = router
            .generate(
                ImageRequest::new("nope", "m", "hi"),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ImageError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn list_models_aggregates_across_impls() {
        let mut router = MultiBackendImageService::new();
        router.register(
            "a",
            Arc::new(Stub {
                backend_id: "alpha",
                models: vec![ModelInfo::new("alpha", "m1", "M1")],
            }),
        );
        router.register(
            "b",
            Arc::new(Stub {
                backend_id: "bravo",
                models: vec![ModelInfo::new("bravo", "m2", "M2")],
            }),
        );
        let models = router.list_models().await.unwrap();
        assert_eq!(models.len(), 2);
    }

    #[tokio::test]
    async fn first_registered_claims_backend_wins() {
        let mut router = MultiBackendImageService::new();
        router.register(
            "a",
            Arc::new(Stub {
                backend_id: "x",
                models: vec![],
            }),
        );
        router.register(
            "b",
            Arc::new(Stub {
                backend_id: "x",
                models: vec![],
            }),
        );
        let resp = router
            .generate(ImageRequest::new("x", "m", "hi"), CancellationToken::new())
            .await
            .unwrap();
        // Both stubs use the backend label in mime_type — the first one ("a"
        // for backend "x") should answer.
        assert_eq!(resp.images[0].mime_type, "image/x");
    }
}
