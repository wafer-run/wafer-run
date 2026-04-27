//! Test that Wafer exposes registered block info via `registered_block_infos()`.

use std::sync::Arc;

use async_trait::async_trait;
use wafer_block::{BlockInfo, ExternalAsset};
use wafer_run::{block::Block, context::Context, types::Message, InputStream, OutputStream, Wafer};

struct StubBlock {
    name: &'static str,
    asset_id: Option<&'static str>,
}

#[async_trait]
impl Block for StubBlock {
    fn info(&self) -> BlockInfo {
        let info = BlockInfo::new(self.name, "0.0.1", "http-handler@v1", "stub");
        if let Some(asset_id) = self.asset_id {
            info.external_assets(vec![ExternalAsset {
                id: asset_id.to_string(),
                loader: "ffmpeg.wasm".to_string(),
                version: "0.0.1".to_string(),
                url: "https://example/asset.wasm".to_string(),
                sha256: "deadbeef".to_string(),
            }])
        } else {
            info
        }
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(Vec::<u8>::new())
    }
}

#[test]
fn registered_block_infos_lists_registered_blocks() {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("empty wafer build is infallible");
    wafer
        .register_block(
            "test/first",
            Arc::new(StubBlock {
                name: "test/first",
                asset_id: None,
            }),
        )
        .expect("register first");
    wafer
        .register_block(
            "test/second",
            Arc::new(StubBlock {
                name: "test/second",
                asset_id: Some("ffmpeg-core"),
            }),
        )
        .expect("register second");

    let infos = wafer.registered_block_infos();
    assert_eq!(infos.len(), 2);
    assert!(infos.iter().any(|i| i.name == "test/first"));
    let second = infos
        .iter()
        .find(|i| i.name == "test/second")
        .expect("second info present");
    assert_eq!(second.external_assets.len(), 1);
    assert_eq!(second.external_assets[0].id, "ffmpeg-core");
}
