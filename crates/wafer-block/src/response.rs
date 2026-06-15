//! Response-construction sugar for the streaming block protocol.
//!
//! Builds `OutputStream` responses purely from wafer-block primitives
//! ([`MetaEntry`], the `META_RESP_*` constants, [`WaferError`]) so blocks in
//! any repo share one implementation instead of hand-assembling meta vectors
//! and error structs at every call site.

use serde::Serialize;

use crate::{
    core_types::{ErrorCode, MetaEntry, WaferError},
    meta::{
        META_RESP_CONTENT_TYPE, META_RESP_COOKIE_PREFIX, META_RESP_HEADER_PREFIX, META_RESP_STATUS,
    },
    streams::output::OutputStream,
};

/// Serialize `value` to JSON and return a successful `OutputStream`
/// with `Content-Type: application/json`.
/// Returns `OutputStream::error(Internal)` if serialization fails.
pub fn ok_json<T: Serialize>(value: &T) -> OutputStream {
    match serde_json::to_vec(value) {
        Ok(body) => OutputStream::respond_with_meta(
            body,
            vec![MetaEntry {
                key: META_RESP_CONTENT_TYPE.to_string(),
                value: "application/json".to_string(),
            }],
        ),
        Err(e) => err_internal("response serialization failed", e),
    }
}

/// Return an empty 200-OK `OutputStream` (no body).
pub fn ok_empty() -> OutputStream {
    OutputStream::respond(vec![])
}

/// Return a 400 Bad Request `OutputStream` ([`ErrorCode::InvalidArgument`]).
pub fn err_bad_request(message: &str) -> OutputStream {
    OutputStream::error(WaferError::new(ErrorCode::InvalidArgument, message))
}

/// Return a 401 Unauthorized `OutputStream` ([`ErrorCode::Unauthenticated`]).
pub fn err_unauthorized(message: &str) -> OutputStream {
    OutputStream::error(WaferError::new(ErrorCode::Unauthenticated, message))
}

/// Return a 403 Forbidden `OutputStream` ([`ErrorCode::PermissionDenied`]).
pub fn err_forbidden(message: &str) -> OutputStream {
    OutputStream::error(WaferError::new(ErrorCode::PermissionDenied, message))
}

/// Return a 404 Not Found `OutputStream` ([`ErrorCode::NotFound`]).
pub fn err_not_found(message: &str) -> OutputStream {
    OutputStream::error(WaferError::new(ErrorCode::NotFound, message))
}

/// Return a 409 Conflict `OutputStream` ([`ErrorCode::AlreadyExists`]).
pub fn err_conflict(message: &str) -> OutputStream {
    OutputStream::error(WaferError::new(ErrorCode::AlreadyExists, message))
}

/// Return a 500 Internal Server Error `OutputStream`.
///
/// **Do not pass raw error text to the client.** This helper generates a short
/// correlation ID, logs the full error detail server-side via
/// [`tracing::error!`], and returns only `"Internal server error (ref: <id>)"`
/// to the caller. The `context` argument is a short, fixed label (no
/// interpolated error text) used for log grouping — e.g. `"Database error"`,
/// `"Storage error"`, `"Failed to update profile"`. The `error` argument is
/// the underlying error/cause; it is logged but NEVER echoed to the client.
///
/// ```ignore
/// .map_err(|e| err_internal("Database error", e))
/// ```
pub fn err_internal<E: std::fmt::Display>(context: &str, error: E) -> OutputStream {
    let id = correlation_id();
    tracing::error!(
        correlation_id = %id,
        error = %error,
        context = %context,
        "internal error",
    );
    OutputStream::error(WaferError::new(
        ErrorCode::Internal,
        format!("Internal server error (ref: {id})"),
    ))
}

/// 500 Internal Server Error for the rare case where there is no underlying
/// cause to log (e.g. an internal invariant violation with a static label).
/// Still logs the context with a correlation ID and returns the sanitized
/// `"Internal server error (ref: <id>)"` message. Most callers should prefer
/// [`err_internal`] which captures the underlying error.
pub fn err_internal_no_cause(context: &str) -> OutputStream {
    let id = correlation_id();
    tracing::error!(
        correlation_id = %id,
        context = %context,
        "internal error (no cause)",
    );
    OutputStream::error(WaferError::new(
        ErrorCode::Internal,
        format!("Internal server error (ref: {id})"),
    ))
}

/// 8-byte hex correlation ID — short enough to be quotable in support tickets,
/// random enough to grep logs without collisions.
fn correlation_id() -> String {
    let mut buf = [0u8; 8];
    if getrandom::getrandom(&mut buf).is_err() {
        // Fall back to timestamp-derived ID; correlation IDs are diagnostic
        // aids, not security primitives, so a deterministic fallback is fine.
        let millis = chrono::Utc::now().timestamp_millis() as u64;
        buf.copy_from_slice(&millis.to_be_bytes());
    }
    crate::hash::hex_encode(&buf)
}

/// Build a response `OutputStream` with custom status, headers, and cookies.
#[derive(Default)]
pub struct ResponseBuilder {
    meta: Vec<MetaEntry>,
    cookie_count: usize,
}

impl ResponseBuilder {
    /// Create a new empty response builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set an explicit HTTP status code (e.g. 201, 301, 302).
    #[must_use]
    pub fn status(mut self, status: u16) -> Self {
        self.meta.push(MetaEntry {
            key: META_RESP_STATUS.to_string(),
            value: status.to_string(),
        });
        self
    }

    /// Add a response header.
    #[must_use]
    pub fn set_header(mut self, key: &str, value: &str) -> Self {
        self.meta.push(MetaEntry {
            key: format!("{META_RESP_HEADER_PREFIX}{key}"),
            value: value.to_string(),
        });
        self
    }

    /// Append a `Set-Cookie` header.
    #[must_use]
    pub fn set_cookie(mut self, cookie: &str) -> Self {
        self.meta.push(MetaEntry {
            key: format!("{}{}", META_RESP_COOKIE_PREFIX, self.cookie_count),
            value: cookie.to_string(),
        });
        self.cookie_count += 1;
        self
    }

    /// Serialise `value` to JSON and emit with Content-Type: application/json.
    pub fn json<T: Serialize>(mut self, value: &T) -> OutputStream {
        match serde_json::to_vec(value) {
            Ok(body) => {
                self.meta.push(MetaEntry {
                    key: META_RESP_CONTENT_TYPE.to_string(),
                    value: "application/json".to_string(),
                });
                OutputStream::respond_with_meta(body, self.meta)
            }
            Err(e) => err_internal("response serialization failed", e),
        }
    }

    /// Emit `bytes` with the given content type (skipped when empty).
    pub fn body(mut self, bytes: Vec<u8>, content_type: &str) -> OutputStream {
        if !content_type.is_empty() {
            self.meta.push(MetaEntry {
                key: META_RESP_CONTENT_TYPE.to_string(),
                value: content_type.to_string(),
            });
        }
        OutputStream::respond_with_meta(bytes, self.meta)
    }

    /// Emit an empty body (headers / cookies only).
    pub fn empty(self) -> OutputStream {
        OutputStream::respond_with_meta(Vec::new(), self.meta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{streams::output::TerminalNotResponse, types::MetaGet};

    #[tokio::test]
    async fn ok_json_sets_json_content_type() {
        let buf = ok_json(&serde_json::json!({"a": 1}))
            .collect_buffered()
            .await
            .expect("respond");
        assert_eq!(buf.body, br#"{"a":1}"#);
        assert_eq!(
            MetaGet::get(&buf.meta, META_RESP_CONTENT_TYPE),
            Some("application/json")
        );
    }

    #[tokio::test]
    async fn ok_empty_has_no_body() {
        let buf = ok_empty().collect_buffered().await.expect("respond");
        assert!(buf.body.is_empty());
    }

    #[tokio::test]
    async fn err_constructors_map_to_error_codes() {
        for (out, code) in [
            (err_bad_request("m"), ErrorCode::InvalidArgument),
            (err_unauthorized("m"), ErrorCode::Unauthenticated),
            (err_forbidden("m"), ErrorCode::PermissionDenied),
            (err_not_found("m"), ErrorCode::NotFound),
            (err_conflict("m"), ErrorCode::AlreadyExists),
        ] {
            match out.collect_buffered().await {
                Err(TerminalNotResponse::Error(e)) => {
                    assert_eq!(e.code, code);
                    assert_eq!(e.message, "m");
                }
                other => panic!("expected error terminal, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn err_internal_sanitizes_message_and_carries_ref() {
        let out = err_internal("Database error", "connection refused to 10.0.0.1");
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::Internal);
                assert!(
                    e.message.starts_with("Internal server error (ref: "),
                    "message: {}",
                    e.message
                );
                assert!(
                    !e.message.contains("connection refused"),
                    "raw cause leaked to client: {}",
                    e.message
                );
            }
            other => panic!("expected error terminal, got {other:?}"),
        }
    }

    /// Drain an `err_internal*` `OutputStream`, assert the sanitized envelope
    /// (`ErrorCode::Internal` + `"Internal server error (ref: <id>)"`), and
    /// return the extracted correlation id.
    async fn ref_id_of(out: OutputStream) -> String {
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::Internal);
                e.message
                    .strip_prefix("Internal server error (ref: ")
                    .and_then(|s| s.strip_suffix(')'))
                    .unwrap_or_else(|| panic!("unexpected message shape: {}", e.message))
                    .to_string()
            }
            other => panic!("expected error terminal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn err_internal_ref_ids_are_unique_across_calls() {
        let a = ref_id_of(err_internal("ctx", "cause a")).await;
        let b = ref_id_of(err_internal("ctx", "cause b")).await;
        assert_ne!(a, b, "correlation ids must differ across calls: {a} == {b}");
    }

    #[tokio::test]
    async fn err_internal_ref_id_is_16_lowercase_hex() {
        let id = ref_id_of(err_internal("ctx", "cause")).await;
        assert_eq!(id.len(), 16, "ref id should be 16 hex chars: {id:?}");
        assert!(
            id.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            "ref id should be lowercase hex: {id:?}"
        );
    }

    #[tokio::test]
    async fn err_internal_no_cause_sanitizes_and_carries_ref() {
        let out = err_internal_no_cause("some secret context");
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::Internal);
                assert!(
                    e.message.starts_with("Internal server error (ref: "),
                    "message: {}",
                    e.message
                );
                assert!(
                    !e.message.contains("some secret context"),
                    "context leaked to client: {}",
                    e.message
                );
                let id = e
                    .message
                    .strip_prefix("Internal server error (ref: ")
                    .and_then(|s| s.strip_suffix(')'))
                    .expect("message shape");
                assert_eq!(
                    id.len(),
                    16,
                    "no-cause ref id should be 16 hex chars: {id:?}"
                );
            }
            other => panic!("expected error terminal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn builder_status_headers_cookies_and_json() {
        let buf = ResponseBuilder::new()
            .status(201)
            .set_header("x-request-id", "abc")
            .set_cookie("session=1; HttpOnly")
            .set_cookie("theme=dark")
            .json(&serde_json::json!({"ok": true}))
            .collect_buffered()
            .await
            .expect("respond");
        assert_eq!(MetaGet::get(&buf.meta, META_RESP_STATUS), Some("201"));
        assert_eq!(
            MetaGet::get(&buf.meta, "resp.header.x-request-id"),
            Some("abc")
        );
        // Cookies are numbered in insertion order.
        assert_eq!(
            MetaGet::get(&buf.meta, "resp.set_cookie.0"),
            Some("session=1; HttpOnly")
        );
        assert_eq!(
            MetaGet::get(&buf.meta, "resp.set_cookie.1"),
            Some("theme=dark")
        );
        assert_eq!(
            MetaGet::get(&buf.meta, META_RESP_CONTENT_TYPE),
            Some("application/json")
        );
    }

    #[tokio::test]
    async fn builder_body_skips_empty_content_type() {
        let buf = ResponseBuilder::new()
            .body(b"raw".to_vec(), "")
            .collect_buffered()
            .await
            .expect("respond");
        assert_eq!(buf.body, b"raw");
        assert_eq!(MetaGet::get(&buf.meta, META_RESP_CONTENT_TYPE), None);
    }

    #[tokio::test]
    async fn builder_empty_emits_meta_only() {
        let buf = ResponseBuilder::new()
            .status(204)
            .empty()
            .collect_buffered()
            .await
            .expect("respond");
        assert!(buf.body.is_empty());
        assert_eq!(MetaGet::get(&buf.meta, META_RESP_STATUS), Some("204"));
    }
}
