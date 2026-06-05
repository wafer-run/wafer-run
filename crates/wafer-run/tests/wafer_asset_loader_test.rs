use std::sync::Arc;

use wafer_run::{
    asset_loader::{AssetLoadStatus, LoadAssetCallback},
    Wafer,
};

struct TestLoader;

#[async_trait::async_trait]
impl LoadAssetCallback for TestLoader {
    async fn load(&self, _: &str) -> AssetLoadStatus {
        AssetLoadStatus::Ready
    }
}

#[tokio::test]
async fn wafer_default_loader_is_noop() {
    let wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    let status = wafer.asset_loader().load("ffmpeg").await;
    assert!(matches!(status, AssetLoadStatus::Failed(_)));
}

#[tokio::test]
async fn wafer_can_register_loader() {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    let loader: Arc<dyn LoadAssetCallback> = Arc::new(TestLoader);
    wafer.set_asset_loader(&loader);
    let status = wafer.asset_loader().load("ffmpeg").await;
    assert!(matches!(status, AssetLoadStatus::Ready));
}
