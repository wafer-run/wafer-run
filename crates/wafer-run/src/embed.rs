//! Shared helpers for embedder bindings (`wafer-ffi`, `wafer-run-node`).
//!
//! Both embedder bindings — `wafer-ffi` for C callers and `wafer-run-node`
//! for Node.js — expose the same behaviors: encoding a collected
//! [`OutputStream`] as an `{"action": ...}` JSON string, registering a block
//! or flow from a file path, and registering a WASM block under a JSON
//! capability bound. They live here, in the runtime crate both bindings
//! already depend on, so the host wire format, the `.wasm`-extension
//! dispatch rule and the capability-bound parsing each have exactly one
//! implementation and one test suite instead of drifting copies per
//! binding.

use wafer_block::{
    core_types::MetaEntry,
    http_codec::{
        classify_response_meta, response_meta_entries, unsendable_kept_meta, InvalidResponseMeta,
        ResponseMetaPart,
    },
    streams::output::{BufferedResponse, TerminalNotResponse},
};

use crate::OutputStream;

/// A terminal whose meta holds an entry no transport can send: the first
/// refused entry, and the `meta` object its `Internal` error carries.
struct Unsendable {
    invalid: InvalidResponseMeta,
    kept: serde_json::Value,
}

impl Unsendable {
    fn new(meta: &[MetaEntry], invalid: InvalidResponseMeta) -> Self {
        Self {
            invalid,
            kept: entries_to_json(unsendable_kept_meta(meta)),
        }
    }
}

/// Entries as a JSON object of string values, keyed by meta key.
fn entries_to_json<'a>(entries: impl IntoIterator<Item = &'a MetaEntry>) -> serde_json::Value {
    entries
        .into_iter()
        .map(|e| (e.key.clone(), serde_json::Value::String(e.value.clone())))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// Project a terminal's meta onto the response entries a host may emit, as a
/// JSON object of string values. See [`output_to_json`] for the contract.
/// `Err` for an unsendable entry (see [`output_to_json`]).
fn response_meta_to_json(meta: &[MetaEntry]) -> Result<serde_json::Value, Unsendable> {
    Ok(entries_to_json(
        response_meta_entries(meta).map_err(|invalid| Unsendable::new(meta, invalid))?,
    ))
}

/// Collect an [`OutputStream`] and encode its terminal as the embedder JSON
/// wire format: `{"action":"respond|error|drop|halt|continue", ...}`.
///
/// Every arm carries `meta`: a JSON object holding **only** the entries
/// [`classify_response_meta`] accepts as response parts, under their own
/// names. That is [`wafer_block::http_codec::response_meta_entries`], the
/// projection the native HTTP boundary applies to the same terminals — so no
/// embedder sees a key an HTTP client would not. The keys, all string-valued:
///
/// - `resp.status` — the status code, as a decimal string.
/// - `resp.content_type` — the `Content-Type`.
/// - `resp.header.{name}` — one response header, `{name}` as the block wrote
///   it (compare it case-insensitively). Any case of `content-type` is the
///   `Content-Type`, and any case of `set-cookie` is one `Set-Cookie`
///   directive.
/// - `resp.set_cookie.{id}` — one `Set-Cookie` directive: the value is the
///   whole directive (`sid=abc; Path=/; HttpOnly`), which a host emits as its
///   own `Set-Cookie` header, never joined with another. `{id}` only keeps
///   two cookies' keys apart; read nothing from it. A block that sets
///   cookies through [`wafer_block::response::ResponseBuilder::set_cookie`]
///   or [`wafer_block::response::cookie_meta`] writes the cookie's identity
///   there ([`wafer_block::http_codec::cookie_meta_key`]): its name, then
///   `;Domain={domain}` and `;Path={path}` when the directive sets them, as
///   in `resp.set_cookie.sid;Path=/api`.
///
/// Everything the projection drops is request or in-flight state: a block
/// builds its terminal from the request message when it needs the headers a
/// middleware set on it (see `wafer-run/cors`'s preflight `Halt`), and that
/// message also carries `http.header.authorization`, `http.header.cookie`,
/// `auth.*` identity, `req.client.ip` and the decoded query.
///
/// - `respond` carries `body` (a UTF-8 string) when the body is valid UTF-8
///   (the common case, human-readable wire shape); when it is not, it
///   carries `body_base64` (Base64-encoded bytes) instead so binary bodies
///   are never silently collapsed to `""`. Exactly one of `body` /
///   `body_base64` is present on a `respond`.
/// - `halt` always uses `body_base64` — Halt may carry non-UTF-8 or empty
///   bodies. Carries `meta` like `respond`.
/// - `error` carries
///   `{"error":{"code":"...","message":"...","detail_code":"..."}}`: `code`
///   is the coarse [`wafer_block::ErrorCode`], `detail_code` the
///   application-level code set via
///   [`wafer_block::WaferError::with_detail_code`], omitted when none was
///   set. Its `meta` sits beside `error`, like every other arm's, so an
///   embedding host can emit the `Retry-After` / `X-RateLimit-*` headers a
///   429 carries.
/// - `drop` carries only `meta` — it maps to a bodiless `204` with those
///   headers and cookies (a flow's drop carries the response headers its
///   middleware set, e.g. CORS). A drop is always a bodiless 204, so its
///   `meta` never holds `resp.status` or `resp.content_type`.
/// - `continue` carries the follow-up message's `kind` plus its `meta`. The
///   message itself does not cross the boundary: a host has nowhere further
///   to forward it, and the flow's in-flight message is not a response.
/// - A stream that ends without a terminal event encodes as an `error` with
///   code `Internal` and empty `meta`.
/// - A terminal whose meta holds an entry no transport can send
///   ([`wafer_block::http_codec::InvalidResponseMetaKind::Unsendable`])
///   encodes as an `error` with code `Internal` whose `meta` holds
///   [`wafer_block::http_codec::unsendable_kept_meta`] — the headers the
///   native HTTP boundary's 500 for the same terminal carries — logged at
///   `error` with the refused key.
pub async fn output_to_json(output: OutputStream) -> String {
    encode_terminal(output.collect_buffered().await).unwrap_or_else(|unsendable| {
        tracing::error!(
            key = %unsendable.invalid.key,
            reason = unsendable.invalid.reason,
            "embedder boundary: response meta cannot be sent; encoding an Internal error"
        );
        serde_json::json!({
            "action": "error",
            "error": { "code": "Internal", "message": "internal server error" },
            "meta": unsendable.kept,
        })
        .to_string()
    })
}

/// [`output_to_json`] for a collected terminal; `Err` for unsendable meta.
fn encode_terminal(
    collected: Result<BufferedResponse, TerminalNotResponse>,
) -> Result<String, Unsendable> {
    Ok(match collected {
        Ok(buf) => {
            let meta_obj = response_meta_to_json(&buf.meta)?;
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
        Err(TerminalNotResponse::Error(err)) => {
            let mut error = serde_json::json!({
                "code": format!("{:?}", err.code),
                "message": err.message,
            });
            if let Some(detail) = err.detail_code() {
                error["detail_code"] = serde_json::Value::String(detail.to_string());
            }
            serde_json::json!({
                "action": "error",
                "error": error,
                "meta": response_meta_to_json(&err.meta)?,
            })
            .to_string()
        }
        Err(TerminalNotResponse::Drop { meta }) => {
            let headers_and_cookies: Vec<MetaEntry> = response_meta_entries(&meta)
                .map_err(|invalid| Unsendable::new(&meta, invalid))?
                .into_iter()
                .filter(|e| {
                    matches!(
                        classify_response_meta(e),
                        Ok(Some(
                            ResponseMetaPart::Header { .. } | ResponseMetaPart::SetCookie(_)
                        ))
                    )
                })
                .cloned()
                .collect();
            serde_json::json!({
                "action": "drop",
                "meta": response_meta_to_json(&headers_and_cookies)?,
            })
            .to_string()
        }
        Err(TerminalNotResponse::Halt(buf)) => {
            use base64ct::{Base64, Encoding};
            let body_b64 = Base64::encode_string(&buf.body);
            serde_json::json!({
                "action": "halt",
                "body_base64": body_b64,
                "meta": response_meta_to_json(&buf.meta)?,
            })
            .to_string()
        }
        Err(TerminalNotResponse::Continue(msg)) => serde_json::json!({
            "action": "continue",
            "kind": msg.kind,
            "meta": response_meta_to_json(&msg.meta)?,
        })
        .to_string(),
        Err(TerminalNotResponse::Malformed) => serde_json::json!({
            "action": "error",
            "error": { "code": "Internal", "message": "stream ended without terminal event" },
            "meta": {},
        })
        .to_string(),
    })
}

/// Register a block or flow definition from a file path.
///
/// If `path` ends with `.wasm`, loads the file as a WASM block and registers
/// it under `name`, which must equal the name the guest reports from
/// `__wafer_info` (registration refuses a mismatch); otherwise reads the file
/// as a WaferFlow JSON definition (the flow's id comes from the JSON itself,
/// not from `name`). This extension-dispatch rule is owned here so every
/// embedder binding resolves paths identically.
///
/// A WASM block registered here has no embedder capability bound: `seal()`
/// bounds it by its `capabilities` block config, which the bindings' static
/// config never sets, so it runs with `BlockCapabilities::none()` whatever it
/// declares. To grant a guest capabilities, register it with
/// [`register_block_path`] instead.
///
/// Errors are returned as display strings ready for the binding's error
/// surface (JSON error string / JS exception).
///
/// Native-only: this reads from the filesystem, so it is gated out on
/// `wasm32` where there is no disk. Its only callers are the native embedder
/// bindings (`wafer-ffi`, `wafer-run-node`); browser/wasm32 embedders load
/// blocks from bytes via `WasmiBlock::load_with_engine` instead (see
/// `runtime::remote`).
#[cfg(all(feature = "wasmi", not(target_arch = "wasm32")))]
pub fn register_path(wafer: &mut crate::Wafer, name: &str, path: &str) -> Result<(), String> {
    if path.ends_with(".wasm") {
        register_wasm(wafer, name, path, None)
    } else {
        let json =
            std::fs::read_to_string(path).map_err(|e| format!("failed to read file: {e}"))?;
        wafer
            .add_flow_json(&json)
            .map_err(|e| format!("invalid WaferFlow JSON: {e}"))
    }
}

/// Register the WASM block at `path` under `name` (the name its guest
/// reports), bounded by `capabilities_json`: a JSON
/// [`BlockCapabilities`](wafer_block::BlockCapabilities) object, the bound
/// [`WasmiBlock::load_with_capabilities`](crate::WasmiBlock::load_with_capabilities)
/// takes. The guest runs under that bound ∩ what it declares; it cannot
/// declare its way past it. A field the object omits denies — `{}` grants
/// nothing — and so does one `BlockCapabilities` does not name, which serde
/// ignores: a misspelt grant fails closed.
///
/// Native-only, for the same reason as [`register_path`].
#[cfg(all(feature = "wasmi", not(target_arch = "wasm32")))]
pub fn register_block_path(
    wafer: &mut crate::Wafer,
    name: &str,
    path: &str,
    capabilities_json: &str,
) -> Result<(), String> {
    let bound = serde_json::from_str(capabilities_json)
        .map_err(|e| format!("invalid capabilities JSON: {e}"))?;
    register_wasm(wafer, name, path, Some(bound))
}

/// Load the WASM file at `path` with the runtime's resource limits, under
/// `bound` when the embedder states one, and register it under `name`.
#[cfg(all(feature = "wasmi", not(target_arch = "wasm32")))]
fn register_wasm(
    wafer: &mut crate::Wafer,
    name: &str,
    path: &str,
    bound: Option<wafer_block::BlockCapabilities>,
) -> Result<(), String> {
    let load_err = |e: &dyn std::fmt::Display| format!("failed to load WASM block: {e}");
    let bytes = std::fs::read(path).map_err(|e| load_err(&e))?;
    let limits = wafer.resource_limits();
    let block = match bound {
        Some(caps) => crate::WasmiBlock::load_with_capabilities_and_limits(&bytes, caps, limits),
        None => crate::WasmiBlock::load_from_bytes_with_limits(&bytes, limits),
    }
    .map_err(|e| load_err(&e))?;
    wafer
        .register_block(name, std::sync::Arc::new(block))
        .map_err(|e| e.to_string())
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
    async fn respond_preserves_response_meta() {
        let out = OutputStream::respond_with_meta(
            b"ok".to_vec(),
            vec![MetaEntry {
                key: "resp.header.x-test".into(),
                value: "1".into(),
            }],
        );
        let json: serde_json::Value = serde_json::from_str(&output_to_json(out).await).unwrap();
        assert_eq!(json["meta"]["resp.header.x-test"], "1");
    }

    /// Meta no transport can send fails the terminal closed, as the HTTP
    /// codec's 500 does: an Internal error keeping the headers that 500
    /// keeps, not a respond missing its CSP.
    #[tokio::test]
    async fn unsendable_meta_encodes_as_an_internal_error() {
        let out = OutputStream::respond_with_meta(
            b"<p>".to_vec(),
            vec![
                MetaEntry {
                    key: "resp.header.X-Frame-Options".into(),
                    value: "DENY".into(),
                },
                MetaEntry {
                    key: "resp.set_cookie.sid".into(),
                    value: "sid=abc".into(),
                },
                MetaEntry {
                    key: "resp.header.Content-Security-Policy".into(),
                    value: "script-src \u{2019}self\u{2019}".into(),
                },
            ],
        );
        let json: serde_json::Value = serde_json::from_str(&output_to_json(out).await).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "action": "error",
                "error": { "code": "Internal", "message": "internal server error" },
                "meta": { "resp.header.X-Frame-Options": "DENY" },
            })
        );
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
    async fn error_terminal_carries_detail_code() {
        let out = OutputStream::error(
            WaferError::new(ErrorCode::InvalidArgument, "x").with_detail_code("auth.invalid_email"),
        );
        let json: serde_json::Value = serde_json::from_str(&output_to_json(out).await).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "action": "error",
                "error": {
                    "code": "InvalidArgument",
                    "message": "x",
                    "detail_code": "auth.invalid_email",
                },
                "meta": {},
            })
        );
    }

    /// Error meta may carry request meta (a block that builds its error from
    /// the request message); only its response entries reach the embedder.
    #[tokio::test]
    async fn error_terminal_projects_error_meta() {
        let mut err = WaferError::new(ErrorCode::ResourceExhausted, "Too many requests");
        err.meta.push(MetaEntry {
            key: "http.header.authorization".into(),
            value: "Bearer SECRET_TOKEN".into(),
        });
        err.meta.push(MetaEntry {
            key: "resp.header.Retry-After".into(),
            value: "30".into(),
        });
        let raw = output_to_json(OutputStream::error(err)).await;
        assert!(!raw.contains("SECRET_TOKEN"), "request meta leaked: {raw}");
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            json["error"],
            serde_json::json!({ "code": "ResourceExhausted", "message": "Too many requests" })
        );
        assert_eq!(
            json["meta"],
            serde_json::json!({ "resp.header.Retry-After": "30" })
        );
    }

    #[tokio::test]
    async fn drop_terminal_carries_only_its_response_meta() {
        let out = OutputStream::drop_request();
        let json: serde_json::Value = serde_json::from_str(&output_to_json(out).await).unwrap();
        assert_eq!(json, serde_json::json!({ "action": "drop", "meta": {} }));

        let out = OutputStream::drop_request_with_meta(vec![
            MetaEntry {
                key: "resp.header.Access-Control-Allow-Origin".into(),
                value: "https://a.example".into(),
            },
            MetaEntry {
                key: "http.header.authorization".into(),
                value: "Bearer SECRET_TOKEN".into(),
            },
            MetaEntry {
                key: "resp.status".into(),
                value: "200".into(),
            },
            MetaEntry {
                key: "resp.content_type".into(),
                value: "text/html".into(),
            },
        ]);
        let json: serde_json::Value = serde_json::from_str(&output_to_json(out).await).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "action": "drop",
                "meta": { "resp.header.Access-Control-Allow-Origin": "https://a.example" },
            })
        );
    }

    #[tokio::test]
    async fn halt_always_base64_encodes_body_and_keeps_response_meta() {
        let out = OutputStream::halt(
            b"stop".to_vec(),
            vec![MetaEntry {
                key: "resp.header.x-halt".into(),
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
        assert_eq!(json["meta"]["resp.header.x-halt"], "y");
    }

    #[tokio::test]
    async fn continue_terminal_carries_kind_and_response_meta() {
        let mut msg = Message::new("next");
        msg.set_meta("resp.header.Vary", "Origin");
        let json: serde_json::Value =
            serde_json::from_str(&output_to_json(OutputStream::continue_with(msg)).await).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "action": "continue",
                "kind": "next",
                "meta": { "resp.header.Vary": "Origin" },
            })
        );
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

        /// A WASM guest whose `__wafer_info` reports `acme/widget`, declaring
        /// `collections: Only[a, c]` and `crypto`, written to `dir`.
        fn widget_guest(dir: &std::path::Path) -> String {
            use std::collections::BTreeSet;

            use wafer_block::{Allowlist, BlockCapabilities, BlockInfo};

            let mut declared = BlockCapabilities::none();
            declared.collections = Allowlist::Only(BTreeSet::from([
                "acme__widget__a".to_string(),
                "acme__widget__c".to_string(),
            ]));
            declared.crypto = true;
            let info = BlockInfo::new("acme/widget", "1.0.0", "handler@v1", "embed fixture")
                .capabilities(declared);
            let json = serde_json::to_string(&info).unwrap();
            let packed = (64u64 << 32) | json.len() as u64;
            let escaped = json.replace('\\', "\\\\").replace('"', "\\\"");
            let wasm = wat::parse_str(format!(
                r#"(module
                    (memory (export "memory") 1)
                    (data (i32.const 64) "{escaped}")
                    (func (export "__wafer_info") (result i64) (i64.const {packed})))"#
            ))
            .unwrap();
            let path = dir.join("widget.wasm");
            std::fs::write(&path, wasm).unwrap();
            path.to_str().unwrap().to_string()
        }

        /// The capabilities `seal()` installed for `acme/widget`.
        async fn sealed_caps(mut w: Wafer) -> wafer_block::BlockCapabilities {
            w.seal().await.expect("seal");
            w.effective_capabilities("acme/widget")
                .cloned()
                .expect("acme/widget has effective capabilities")
        }

        /// The embedder's bound caps what the guest declared: it gets
        /// `bound ∩ declared`, not its whole declaration.
        #[tokio::test]
        async fn register_block_path_bounds_the_guest_by_the_embedder_capabilities() {
            let dir = tempfile::tempdir().unwrap();
            let path = widget_guest(dir.path());
            let mut w = wafer();
            super::super::register_block_path(
                &mut w,
                "acme/widget",
                &path,
                r#"{"collections":{"Only":["acme__widget__a","acme__widget__b"]}}"#,
            )
            .expect("registers");

            let mut expected = wafer_block::BlockCapabilities::none();
            expected.collections =
                wafer_block::Allowlist::Only(["acme__widget__a".to_string()].into());
            assert_eq!(sealed_caps(w).await, expected);
        }

        /// Without a bound the guest gets nothing, whatever it declares: the
        /// bindings cannot state its `capabilities` config.
        #[tokio::test]
        async fn register_path_leaves_a_wasm_guest_without_capabilities() {
            let dir = tempfile::tempdir().unwrap();
            let path = widget_guest(dir.path());
            let mut w = wafer();
            super::super::register_path(&mut w, "acme/widget", &path).expect("registers");
            assert_eq!(sealed_caps(w).await, wafer_block::BlockCapabilities::none());
        }

        #[test]
        fn register_block_path_refuses_invalid_capabilities_json() {
            let dir = tempfile::tempdir().unwrap();
            let path = widget_guest(dir.path());
            let mut w = wafer();
            let err = super::super::register_block_path(
                &mut w,
                "acme/widget",
                &path,
                r#"{"collections":"All"}"#,
            )
            .expect_err("`All` is not an Allowlist");
            assert!(err.starts_with("invalid capabilities JSON:"), "got: {err}");
            assert!(!w.has_block("acme/widget"), "nothing registered on refusal");
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
