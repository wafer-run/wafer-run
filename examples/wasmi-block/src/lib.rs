use wafer_sdk::*;

struct EchoBlock;

#[wafer_block(
    name = "example/echo",
    version = "0.1.0",
    interface = "handler@v1",
    summary = "Echo block for wasmi testing"
)]
impl EchoBlock {
    fn handle(msg: Message, _body: Vec<u8>) -> GuestResult {
        let response_data = serde_json::to_vec(&serde_json::json!({
            "echo": true,
            "kind": msg.kind,
            "meta_count": msg.meta.len(),
        }))
        .unwrap();

        GuestResult::respond_with_meta(
            response_data,
            vec![MetaEntry {
                key: "content-type".to_string(),
                value: "application/json".to_string(),
            }],
        )
    }
}
