use std::sync::Arc;
use wafer_run::asset_loader::{
    AssetLoadError, AssetLoadStatus, LoadAssetCallback, NoopAssetLoader,
};

#[test]
fn noop_returns_failed() {
    let loader: Arc<dyn LoadAssetCallback> = Arc::new(NoopAssetLoader);
    let status = futures::executor::block_on(loader.load("anything"));
    assert!(matches!(status, AssetLoadStatus::Failed(_)));
}

#[test]
fn custom_callback_is_invoked() {
    struct Counting {
        count: std::sync::Mutex<u32>,
    }
    #[async_trait::async_trait]
    impl LoadAssetCallback for Counting {
        async fn load(&self, _id: &str) -> AssetLoadStatus {
            *self.count.lock().unwrap() += 1;
            AssetLoadStatus::Ready
        }
    }

    let loader = Arc::new(Counting {
        count: std::sync::Mutex::new(0),
    });
    let status = futures::executor::block_on(loader.load("ffmpeg"));
    assert!(matches!(status, AssetLoadStatus::Ready));
    assert_eq!(*loader.count.lock().unwrap(), 1);
}

#[test]
fn error_kind_carries_message() {
    let err = AssetLoadError::sha_mismatch("ffmpeg", "expected", "got");
    let s = err.to_string();
    assert!(s.contains("sha"));
    assert!(s.contains("ffmpeg"));
}
