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
            name: "@wafer-site/playground".to_string(),
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

const RUST_TEMPLATE: &str = r#"use std::collections::HashMap;

fn main() {
    println!("=== WAFER Block Example ===\n");

    // Simulate incoming request headers
    let mut headers = HashMap::new();
    headers.insert("content-type", "application/json");
    headers.insert("x-request-id", "req-42");
    headers.insert("x-wafer-flow", "main");

    println!("Incoming message:");
    for (key, value) in &headers {
        println!("  {}: {}", key, value);
    }

    // Block processing
    let items: Vec<i32> = (1..=5).map(|x| x * x).collect();
    println!("\nProcessed: {:?}", items);
    let sum: i32 = items.iter().sum();
    println!("Sum of squares: {}", sum);

    // Response
    println!("\nBlock response:");
    println!("  {{\"status\": 200, \"hello\": \"world\", \"sum\": {}}}", sum);
}
"#;

const GO_TEMPLATE: &str = r#"package main

import (
	"encoding/json"
	"fmt"
)

type Message struct {
	Path   string `json:"path"`
	Method string `json:"method"`
	Data   string `json:"data"`
}

type Response struct {
	Status int         `json:"status"`
	Body   interface{} `json:"body"`
}

func main() {
	fmt.Println("=== WAFER Block Example ===")
	fmt.Println()

	// Simulate incoming message
	msg := Message{
		Path:   "/api/hello",
		Method: "GET",
		Data:   "Hello from playground",
	}

	msgJSON, _ := json.MarshalIndent(msg, "", "  ")
	fmt.Printf("Incoming message:\n%s\n", string(msgJSON))

	// Process and respond
	resp := Response{
		Status: 200,
		Body: map[string]interface{}{
			"hello": "world",
			"items": []int{1, 4, 9, 16, 25},
		},
	}

	respJSON, _ := json.MarshalIndent(resp, "", "  ")
	fmt.Printf("\nBlock response:\n%s\n", string(respJSON))
}
"#;

const JS_TEMPLATE: &str = r#"// WAFER Block Example — JavaScript/Node.js

class HelloBlock {
    info() {
        return {
            name: "hello",
            version: "1.0.0",
            interface: "handler@v1",
            summary: "Says hello",
        };
    }

    handle(message) {
        console.log("=== WAFER Block Example ===\n");

        console.log("Incoming message:");
        console.log("  path:", message.path);
        console.log("  method:", message.method);
        console.log("  data:", message.data);

        // Process
        const items = [1, 2, 3, 4, 5].map(x => x * x);
        console.log("\nProcessed:", items);
        const sum = items.reduce((a, b) => a + b, 0);
        console.log("Sum of squares:", sum);

        // Respond
        const response = {
            status: 200,
            body: { hello: "world", sum },
        };

        console.log("\nBlock response:");
        console.log(JSON.stringify(response, null, 2));
        return response;
    }
}

// Run the block
const block = new HelloBlock();
console.log("Block:", JSON.stringify(block.info(), null, 2));
console.log();

block.handle({
    path: "/api/hello",
    method: "GET",
    data: "Hello from playground",
});
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
        "@wafer-site/playground",
        Arc::new(PlaygroundBlock::new()),
    );
}
