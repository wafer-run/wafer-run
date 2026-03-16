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
        BlockInfo {
            name: "wafer-site/playground".to_string(),
            version: "0.1.0".to_string(),
            interface: "handler@v1".to_string(),
            summary: "Browser-based code editor with live execution".to_string(),
            instance_mode: InstanceMode::Singleton,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: wafer_run::types::BlockRuntime::Native,
            requires: Vec::new(),
        }
    }

    async fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let path = msg.path().to_string();
        let action = msg.action().to_string();

        match (action.as_str(), path.as_str()) {
            // Serve playground page
            (_, "/playground") | (_, "/playground/") => {
                let html = include_str!("../content/playground.html");
                respond(msg, html.as_bytes().to_vec(), "text/html")
            }

            // --- Proxy: Rust Playground ---
            ("create", "/playground/run/rust") => {
                let body: serde_json::Value = match msg.decode() {
                    Ok(v) => v,
                    Err(_) => return err_bad_request(msg, "Invalid JSON body"),
                };

                let source = body
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if source.is_empty() {
                    return err_bad_request(msg, "No source code provided");
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

                match proxy_post_json("https://play.rust-lang.org/execute", &payload) {
                    Ok(bytes) => respond(
                        msg,
                        bytes,
                        "application/json",
                    ),
                    Err(e) => error(
                        msg,
                        "unavailable",
                        &format!("Rust Playground error: {}", e),
                    ),
                }
            }

            // --- Proxy: Go Playground ---
            ("create", "/playground/run/go") => {
                let body: serde_json::Value = match msg.decode() {
                    Ok(v) => v,
                    Err(_) => return err_bad_request(msg, "Invalid JSON body"),
                };

                let source = body
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if source.is_empty() {
                    return err_bad_request(msg, "No source code provided");
                }

                match proxy_post_form(
                    "https://go.dev/_/compile",
                    &[("version", "2"), ("body", &source), ("withVet", "true")],
                ) {
                    Ok(bytes) => respond(
                        msg,
                        bytes,
                        "application/json",
                    ),
                    Err(e) => error(
                        msg,
                        "unavailable",
                        &format!("Go Playground error: {}", e),
                    ),
                }
            }

            // --- Templates ---
            ("retrieve", "/playground/templates/rust") => json_respond(
                msg,
                &serde_json::json!({ "language": "rust", "template": RUST_TEMPLATE }),
            ),

            ("retrieve", "/playground/templates/go") => json_respond(
                msg,
                &serde_json::json!({ "language": "go", "template": GO_TEMPLATE }),
            ),

            ("retrieve", "/playground/templates/javascript") => json_respond(
                msg,
                &serde_json::json!({ "language": "javascript", "template": JS_TEMPLATE }),
            ),

            _ => err_not_found(
                msg,
                &format!("Playground endpoint not found: {}", path),
            ),
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
// In a real block you'd use the wafer-sdk-go package.
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
// In a real block you'd use the wafer-sdk-ts package.

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

/// Proxy a JSON POST request using async reqwest, bridged via block_in_place.
fn proxy_post_json(url: &str, payload: &serde_json::Value) -> Result<Vec<u8>, String> {
    let handle = tokio::runtime::Handle::current();
    let url = url.to_string();
    let body = payload.to_string();
    tokio::task::block_in_place(|| {
        handle.block_on(async {
            let client = playground_client();
            let resp = client
                .post(&url)
                .header("Content-Type", "application/json")
                .body(body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            resp.bytes().await.map(|b| b.to_vec()).map_err(|e| e.to_string())
        })
    })
}

/// Proxy a form-encoded POST request using async reqwest, bridged via block_in_place.
fn proxy_post_form(url: &str, params: &[(&str, &str)]) -> Result<Vec<u8>, String> {
    let handle = tokio::runtime::Handle::current();
    let url = url.to_string();
    let params: Vec<(String, String)> = params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    tokio::task::block_in_place(|| {
        handle.block_on(async {
            let client = playground_client();
            let resp = client
                .post(&url)
                .form(&params)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            resp.bytes().await.map(|b| b.to_vec()).map_err(|e| e.to_string())
        })
    })
}

pub fn register(w: &mut Wafer) {
    w.register_block(
        "wafer-site/playground",
        Arc::new(PlaygroundBlock::new()),
    );
}
