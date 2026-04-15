//! Playground block: browser-based code editor with live execution.
//!
//! POST /playground/run/rust - proxy to Rust Playground (play.rust-lang.org)
//! POST /playground/run/go   - proxy to Go Playground (go.dev)
//! GET  /playground/templates/{lang} - get template code per language

use std::sync::Arc;
use wafer_run::*;

pub struct PlaygroundBlock;

impl PlaygroundBlock {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Block for PlaygroundBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-site/playground",
            "0.0.1",
            "http-handler@v1",
            "Browser-based code editor with live execution",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let path = msg.path().to_string();
        let action = msg.action().to_string();

        match (action.as_str(), path.as_str()) {
            // Serve playground page
            (_, "/playground") | (_, "/playground/") => {
                let html = include_str!("../content/playground.html");
                OutputStream::respond(html.as_bytes().to_vec())
            }

            // --- Proxy: Rust Playground ---
            ("create", "/playground/run/rust") => {
                let body_bytes = input.collect_to_bytes().await;
                let body: serde_json::Value = match serde_json::from_slice(&body_bytes) {
                    Ok(v) => v,
                    Err(_) => {
                        return OutputStream::error(WaferError {
                            code: ErrorCode::InvalidArgument,
                            message: "Invalid JSON body".to_string(),
                            meta: vec![],
                        })
                    }
                };

                let source = body
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if source.is_empty() {
                    return OutputStream::error(WaferError {
                        code: ErrorCode::InvalidArgument,
                        message: "No source code provided".to_string(),
                        meta: vec![],
                    });
                }

                let payload = serde_json::json!({
                    "channel": "stable",
                    "mode": "debug",
                    "edition": "2021",
                    "crateType": "bin",
                    "tests": false,
                    "code": source,
                    "backtrace": false
                });

                match proxy_post_json("https://play.rust-lang.org/execute", &payload).await {
                    Ok(bytes) => OutputStream::respond(bytes),
                    Err(e) => OutputStream::error(WaferError {
                        code: ErrorCode::Internal,
                        message: format!("Rust Playground error: {}", e),
                        meta: vec![],
                    }),
                }
            }

            // --- Proxy: Go Playground ---
            ("create", "/playground/run/go") => {
                let body_bytes = input.collect_to_bytes().await;
                let body: serde_json::Value = match serde_json::from_slice(&body_bytes) {
                    Ok(v) => v,
                    Err(_) => {
                        return OutputStream::error(WaferError {
                            code: ErrorCode::InvalidArgument,
                            message: "Invalid JSON body".to_string(),
                            meta: vec![],
                        })
                    }
                };

                let source = body
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if source.is_empty() {
                    return OutputStream::error(WaferError {
                        code: ErrorCode::InvalidArgument,
                        message: "No source code provided".to_string(),
                        meta: vec![],
                    });
                }

                match proxy_post_form(
                    "https://go.dev/_/compile",
                    &[("version", "2"), ("body", &source), ("withVet", "true")],
                )
                .await
                {
                    Ok(bytes) => OutputStream::respond(bytes),
                    Err(e) => OutputStream::error(WaferError {
                        code: ErrorCode::Internal,
                        message: format!("Go Playground error: {}", e),
                        meta: vec![],
                    }),
                }
            }

            // --- Templates ---
            ("retrieve", "/playground/templates/rust") => {
                let body = serde_json::to_vec(&serde_json::json!({
                    "language": "rust",
                    "template": RUST_TEMPLATE,
                }))
                .unwrap_or_default();
                OutputStream::respond(body)
            }

            ("retrieve", "/playground/templates/go") => {
                let body = serde_json::to_vec(&serde_json::json!({
                    "language": "go",
                    "template": GO_TEMPLATE,
                }))
                .unwrap_or_default();
                OutputStream::respond(body)
            }

            ("retrieve", "/playground/templates/javascript") => {
                let body = serde_json::to_vec(&serde_json::json!({
                    "language": "javascript",
                    "template": JS_TEMPLATE,
                }))
                .unwrap_or_default();
                OutputStream::respond(body)
            }

            _ => OutputStream::error(WaferError {
                code: ErrorCode::NotFound,
                message: format!("Playground endpoint not found: {}", path),
                meta: vec![],
            }),
        }
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

const RUST_TEMPLATE: &str = r#"// A wafer block: receives a message, returns a response.
// In a real block you'd use #[block] from wafer-sdk.

fn handle(name: &str) -> String {
    format!("{{\n  \"greeting\": \"Hello, {}!\"\n}}", name)
}

fn main() {
    println!("Input:  {{\"name\": \"world\"}}");
    let output = handle("world");
    println!("Output: {}", output);
}
"#;

const GO_TEMPLATE: &str = r#"// A wafer block: receives a message, returns a response.
// In a real block you'd use the wafer-sdk-go SDK (sdks/go/).
package main

import "fmt"

func handle(name string) string {
	return fmt.Sprintf(`{"greeting": "Hello, %s!"}`, name)
}

func main() {
	input  := `{"name": "world"}`
	output := handle("world")

	fmt.Println("Input: ", input)
	fmt.Println("Output:", output)
}
"#;

const JS_TEMPLATE: &str = r#"// A wafer block: receives a message, returns a response.
// In a real block you'd use the @wafer-run/sdk package (sdks/js/).

function handle(input) {
    return { greeting: "Hello, " + input.name + "!" };
}

const input  = { name: "world" };
const output = handle(input);

console.log("Input: ", JSON.stringify(input));
console.log("Output:", JSON.stringify(output));
"#;

/// Shared HTTP client for playground proxy requests — avoids per-request TLS setup.
fn playground_client() -> &'static reqwest::Client {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// Proxy a JSON POST request using async reqwest.
async fn proxy_post_json(url: &str, payload: &serde_json::Value) -> Result<Vec<u8>, String> {
    let client = playground_client();
    let resp = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(payload.to_string())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| e.to_string())
}

/// Proxy a form-encoded POST request using async reqwest.
async fn proxy_post_form(url: &str, params: &[(&str, &str)]) -> Result<Vec<u8>, String> {
    let client = playground_client();
    let params: Vec<(&str, &str)> = params.to_vec();
    let resp = client
        .post(url)
        .form(&params)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| e.to_string())
}

pub fn register(w: &mut Wafer) -> Result<(), String> {
    w.register_block("wafer-site/playground", Arc::new(PlaygroundBlock::new()))
}
