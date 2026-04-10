use wafer_sdk::*;

struct EchoBlock;

#[wafer_block(
    name = "example/echo",
    version = "0.1.0",
    interface = "handler@v1",
    summary = "Echo block for wasmi testing"
)]
impl EchoBlock {
    fn handle(msg: Message) -> BlockResult {
        let response_data = serde_json::to_vec(&serde_json::json!({
            "echo": true,
            "kind": msg.kind,
            "meta_count": msg.meta.len(),
        }))
        .unwrap();

        BlockResult {
            action: Action::Respond,
            response: Some(Response {
                data: response_data,
                meta: vec![MetaEntry {
                    key: "content-type".to_string(),
                    value: "application/json".to_string(),
                }],
            }),
            error: None,
            message: None,
        }
    }
}
