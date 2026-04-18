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

#[test]
fn wafer_default_loader_is_noop() {
    let wafer = Wafer::new();
    let status = futures::executor::block_on(wafer.asset_loader().load("ffmpeg"));
    assert!(matches!(status, AssetLoadStatus::Failed(_)));
}

#[test]
fn wafer_can_register_loader() {
    let mut wafer = Wafer::new();
    wafer.set_asset_loader(Arc::new(TestLoader));
    let status = futures::executor::block_on(wafer.asset_loader().load("ffmpeg"));
    assert!(matches!(status, AssetLoadStatus::Ready));
}
