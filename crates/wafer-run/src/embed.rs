//! Shared helpers for embedder bindings (`wafer-ffi`, `wafer-run-node`).
//!
//! Both embedder bindings — `wafer-ffi` for C callers and `wafer-run-node`
//! for Node.js — expose the same two behaviors: encoding a collected
//! [`OutputStream`] as an `{"action": ...}` JSON string, and registering a
//! block or flow from a file path. They live here, in the runtime crate both
//! bindings already depend on, so the host wire format and the
//! `.wasm`-extension dispatch rule each have exactly one implementation and
//! one test suite instead of drifting copies per binding.

use wafer_block::{core_types::MetaEntry, streams::output::TerminalNotResponse};

use crate::OutputStream;

/// Convert collected meta entries into a JSON object of string values.
fn meta_to_json(meta: &[MetaEntry]) -> serde_json::Value {
    meta.iter()
        .map(|e| (e.key.clone(), serde_json::Value::String(e.value.clone())))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// Collect an [`OutputStream`] and encode its terminal as the embedder JSON
/// wire format: `{"action":"respond|error|drop|halt|continue", ...}`.
///
/// - `respond` carries `body` (a UTF-8 string) when the body is valid UTF-8
///   (the common case, human-readable wire shape); when it is not, it
///   carries `body_base64` (Base64-encoded bytes) instead so binary bodies
///   are never silently collapsed to `""`. Exactly one of `body` /
///   `body_base64` is present on a `respond`. `meta` is a JSON object of
///   string values.
/// - `halt` always uses `body_base64` — Halt may carry non-UTF-8 or empty
///   bodies. Carries `meta` like `respond`.
/// - `error` carries `{"error":{"code":"...","message":"..."}}`.
/// - `drop` carries no payload.
/// - `continue` carries the follow-up `message` as a JSON object.
/// - A stream that ends without a terminal event encodes as an `error` with
///   code `Internal`.
pub async fn output_to_json(output: OutputStream) -> String {
    match output.collect_buffered().await {
        Ok(buf) => {
            let meta_obj = meta_to_json(&buf.meta);
            match String::from_utf8(buf.body) {
                Ok(body_str) => serde_json::json!({
                    "action": "respond",
                    "body": body_str,
                    "meta": meta_obj,
                })
                .to_string(),
                Err(e) => {
                    use base64ct::{Base64, Encoding};
                    let body_b64 = Base64::encode_string(e.as_bytes());
                    serde_json::json!({
                        "action": "respond",
                        "body_base64": body_b64,
                        "meta": meta_obj,
                    })
                    .to_string()
                }
            }
        }
        Err(TerminalNotResponse::Error(err)) => serde_json::json!({
            "action": "error",
            "error": {
                "code": format!("{:?}", err.code),
                "message": err.message,
            }
        })
        .to_string(),
        Err(TerminalNotResponse::Drop) => serde_json::json!({ "action": "drop" }).to_string(),
        Err(TerminalNotResponse::Halt(buf)) => {
            use base64ct::{Base64, Encoding};
            let body_b64 = Base64::encode_string(&buf.body);
            serde_json::json!({
                "action": "halt",
                "body_base64": body_b64,
                "meta": meta_to_json(&buf.meta),
            })
            .to_string()
        }
        Err(TerminalNotResponse::Continue(msg)) => serde_json::json!({
            "action": "continue",
            "message": serde_json::to_value(&msg).unwrap_or_default(),
        })
        .to_string(),
        Err(TerminalNotResponse::Malformed) => serde_json::json!({
            "action": "error",
            "error": { "code": "Internal", "message": "stream ended without terminal event" }
        })
        .to_string(),
    }
}

/// Register a block or flow definition from a file path.
///
/// If `path` ends with `.wasm`, loads the file as a WASM block and registers
/// it under `name`; otherwise reads the file as a WaferFlow JSON definition
/// (the flow's id comes from the JSON itself, not from `name`). This
/// extension-dispatch rule is owned here so every embedder binding resolves
/// paths identically.
///
/// Errors are returned as display strings ready for the binding's error
/// surface (JSON error string / JS exception).
///
/// Native-only: this reads from the filesystem (`WasmiBlock::load` and
/// `std::fs::read_to_string`), so it is gated out on `wasm32` where there is
/// no disk. Its only callers are the native embedder bindings (`wafer-ffi`,
/// `wafer-run-node`); browser/wasm32 embedders load blocks from bytes via
/// `WasmiBlock::load_with_engine` instead (see `runtime::remote`).
#[cfg(all(feature = "wasmi", not(target_arch = "wasm32")))]
pub fn register_path(wafer: &mut crate::Wafer, name: &str, path: &str) -> Result<(), String> {
    if path.ends_with(".wasm") {
        let block =
            crate::WasmiBlock::load(path).map_err(|e| format!("failed to load WASM block: {e}"))?;
        wafer
            .register_block(name, std::sync::Arc::new(block))
            .map_err(|e| e.to_string())
    } else {
        let json =
            std::fs::read_to_string(path).map_err(|e| format!("failed to read file: {e}"))?;
        wafer
            .add_flow_json(&json)
            .map_err(|e| format!("invalid WaferFlow JSON: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use wafer_block::core_types::{ErrorCode, WaferError};

    use super::*;
    use crate::Message;

    #[tokio::test]
    async fn respond_utf8_body_uses_body_field() {
        let out = OutputStream::respond(b"hello".to_vec());
        let json: serde_json::Value = serde_json::from_str(&output_to_json(out).await).unwrap();
        assert_eq!(json["action"], "respond");
        assert_eq!(json["body"], "hello");
        assert!(json.get("body_base64").is_none());
    }

    #[tokio::test]
    async fn respond_non_utf8_body_uses_body_base64_not_empty_string() {
        // Lone 0xFF is invalid UTF-8 — a naive `unwrap_or_default()` would
        // collapse this to `body: ""` (silent data loss). It must round-trip
        // as base64 and not appear under `body`.
        let bytes = vec![0xFF, 0x00, 0x42];
        let out = OutputStream::respond(bytes.clone());
        let json: serde_json::Value = serde_json::from_str(&output_to_json(out).await).unwrap();
        assert_eq!(json["action"], "respond");
        assert!(
            json.get("body").is_none(),
            "binary body must not be under `body`"
        );
        use base64ct::{Base64, Encoding};
        let b64 = json["body_base64"].as_str().expect("body_base64 present");
        assert_eq!(Base64::decode_vec(b64).unwrap(), bytes);
    }

    #[tokio::test]
    async fn respond_empty_body_is_empty_string_not_base64() {
        // A genuinely-empty UTF-8 body stays `body: ""` — distinguishable from
        // a non-UTF-8 body, which would use `body_base64`.
        let out = OutputStream::respond(Vec::new());
        let json: serde_json::Value = serde_json::from_str(&output_to_json(out).await).unwrap();
        assert_eq!(json["action"], "respond");
        assert_eq!(json["body"], "");
        assert!(json.get("body_base64").is_none());
    }

    #[tokio::test]
    async fn respond_preserves_meta() {
        let out = OutputStream::respond_with_meta(
            b"ok".to_vec(),
            vec![MetaEntry {
                key: "x-test".into(),
                value: "1".into(),
            }],
        );
        let json: serde_json::Value = serde_json::from_str(&output_to_json(out).await).unwrap();
        assert_eq!(json["meta"]["x-test"], "1");
    }

    #[tokio::test]
    async fn error_terminal_carries_code_and_message() {
        let out = OutputStream::error(WaferError {
            code: ErrorCode::NotFound,
            message: "missing".into(),
            meta: vec![],
        });
        let json: serde_json::Value = serde_json::from_str(&output_to_json(out).await).unwrap();
        assert_eq!(json["action"], "error");
        assert_eq!(json["error"]["code"], "NotFound");
        assert_eq!(json["error"]["message"], "missing");
    }

    #[tokio::test]
    async fn drop_terminal_is_action_only() {
        let out = OutputStream::drop_request();
        let json: serde_json::Value = serde_json::from_str(&output_to_json(out).await).unwrap();
        assert_eq!(json, serde_json::json!({ "action": "drop" }));
    }

    #[tokio::test]
    async fn halt_always_base64_encodes_body_and_keeps_meta() {
        let out = OutputStream::halt(
            b"stop".to_vec(),
            vec![MetaEntry {
                key: "x-halt".into(),
                value: "y".into(),
            }],
        );
        let json: serde_json::Value = serde_json::from_str(&output_to_json(out).await).unwrap();
        assert_eq!(json["action"], "halt");
        assert!(
            json.get("body").is_none(),
            "halt must never carry a `body` field"
        );
        use base64ct::{Base64, Encoding};
        let b64 = json["body_base64"].as_str().expect("body_base64 present");
        assert_eq!(Base64::decode_vec(b64).unwrap(), b"stop");
        assert_eq!(json["meta"]["x-halt"], "y");
    }

    #[tokio::test]
    async fn continue_terminal_carries_message_object() {
        let out = OutputStream::continue_with(Message::new("next"));
        let json: serde_json::Value = serde_json::from_str(&output_to_json(out).await).unwrap();
        assert_eq!(json["action"], "continue");
        assert_eq!(json["message"]["kind"], "next");
    }

    #[cfg(feature = "wasmi")]
    mod register_path {
        use std::{io::Write, sync::Arc};

        use crate::{StaticConfigSource, Wafer};

        fn wafer() -> Wafer {
            Wafer::new(Arc::new(StaticConfigSource::default())).expect("Wafer::new")
        }

        #[test]
        fn non_wasm_path_registers_flow_json() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("flow.json");
            std::fs::write(
                &path,
                r#"{
                    "id": "from-path",
                    "name": "From Path",
                    "version": "0.1.0",
                    "steps": [{ "id": "root", "block": "test/echo" }]
                }"#,
            )
            .unwrap();

            let mut w = wafer();
            super::super::register_path(&mut w, "ignored-for-flows", path.to_str().unwrap())
                .expect("flow registration should succeed");
            assert!(
                w.flows_info().iter().any(|f| f.id == "from-path"),
                "flow id must come from the JSON definition"
            );
        }

        #[test]
        fn missing_file_reports_read_failure() {
            let mut w = wafer();
            let err = super::super::register_path(&mut w, "f", "/nonexistent/flow.json")
                .expect_err("missing file must fail");
            assert!(err.starts_with("failed to read file:"), "got: {err}");
        }

        #[test]
        fn invalid_flow_json_reports_flow_error() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("bad.json");
            std::fs::write(&path, "not json").unwrap();

            let mut w = wafer();
            let err = super::super::register_path(&mut w, "f", path.to_str().unwrap())
                .expect_err("invalid flow JSON must fail");
            assert!(err.starts_with("invalid WaferFlow JSON:"), "got: {err}");
        }

        #[test]
        fn wasm_extension_dispatches_to_block_loader() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("bad.wasm");
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(b"definitely not wasm").unwrap();
            drop(f);

            let mut w = wafer();
            let err = super::super::register_path(&mut w, "test/bad", path.to_str().unwrap())
                .expect_err("garbage wasm must fail to load");
            assert!(err.starts_with("failed to load WASM block:"), "got: {err}");
        }
    }
}
