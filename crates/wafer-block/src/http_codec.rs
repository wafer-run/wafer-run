//! Transport-agnostic HTTP ↔ [`Message`] protocol codec.
//!
//! The **single implementation** of the HTTP-to-WAFER protocol mapping. Every
//! HTTP adapter — the native axum listener (`wafer-block-http-listener`), the
//! `wafer-run/router` block's config tokens, and the solobase Cloudflare /
//! browser adapters — delegates here instead of carrying its own copy of the
//! method→action table, the request-meta layout, the response-meta
//! classifier, or the `ErrorCode`→status map.
//!
//! Layers, inbound to outbound:
//!
//! 1. [`action_for_http_method`] / [`try_action_for_http_method`] — the one
//!    wire-contract table mapping HTTP methods to [`RequestAction`] names.
//! 2. [`build_http_message`] — platform-neutral request-head → [`Message`]
//!    builder (callers adapt their header type to an iterator of pairs).
//! 3. [`classify_response_meta`] / [`response_meta_parts`] — per-entry
//!    response-meta classifier usable by both buffered and streaming
//!    consumers (streaming adapters classify each `Meta` event as it
//!    arrives; no collection required).
//! 4. [`error_code_to_http_status`], [`resolve_status`],
//!    [`resolve_error_status`] — status resolution: explicit
//!    [`META_RESP_STATUS`] override wins, then error-code-derived, then the
//!    caller's default.
//! 5. [`collect_http_response`] / [`buffered_to_http_response`] — buffered
//!    terminal-event mapping from an [`OutputStream`] to a transport-neutral
//!    [`HttpResponseParts`] that thin platform glue turns into an
//!    `axum`/`worker`/`web_sys` response.
//!
//! ## Canonical meta keys
//!
//! Only the canonical response meta keys from [`crate::meta`] are honored:
//! [`META_RESP_STATUS`], [`META_RESP_HEADER_PREFIX`]`*`,
//! [`META_RESP_COOKIE_PREFIX`]`*`, [`META_RESP_CONTENT_TYPE`]. Legacy aliases
//! that pre-consolidation adapters tolerated (`http.status`, `resp.cookie.*`,
//! `http.resp.header.*`, `http.resp.set-cookie.*`, a literal `Content-Type`
//! meta key) are **not** recognized — same key, same format, everywhere. See
//! the `legacy_keys_are_ignored` test for the pinned drift table.

use crate::{
    core_types::{ErrorCode, Message, MetaEntry, WaferError},
    meta::{
        META_REQ_ACTION, META_REQ_CLIENT_IP, META_REQ_CONTENT_TYPE, META_REQ_QUERY_PREFIX,
        META_REQ_RESOURCE, META_RESP_CONTENT_TYPE, META_RESP_COOKIE_PREFIX,
        META_RESP_HEADER_PREFIX, META_RESP_STATUS,
    },
    streams::output::{BufferedResponse, OutputStream, TerminalNotResponse},
    types::{MetaGet, RequestAction},
};

// ---------------------------------------------------------------------------
// HTTP-transport meta keys (`http.*` family)
// ---------------------------------------------------------------------------

/// Raw HTTP method, normalized to uppercase (e.g. `GET`).
pub const META_HTTP_METHOD: &str = "http.method";
/// Request URI path as received (e.g. `/orgs/123`).
pub const META_HTTP_PATH: &str = "http.path";
/// Raw (undecoded) query string, without the leading `?`.
pub const META_HTTP_RAW_QUERY: &str = "http.raw_query";
/// Remote peer address as observed by the transport.
pub const META_HTTP_REMOTE_ADDR: &str = "http.remote_addr";
/// Request `Content-Type` header value (also mirrored to
/// [`META_REQ_CONTENT_TYPE`]).
pub const META_HTTP_CONTENT_TYPE: &str = "http.content_type";
/// Request `Host` header value.
pub const META_HTTP_HOST: &str = "http.host";
/// Prefix for raw request headers (`http.header.{lowercased-name}`).
pub const META_HTTP_HEADER_PREFIX: &str = "http.header.";
/// Prefix for decoded query parameters (`http.query.{name}`; also mirrored
/// to [`META_REQ_QUERY_PREFIX`]`{name}`).
pub const META_HTTP_QUERY_PREFIX: &str = "http.query.";

/// Default response `Content-Type` applied when a response carries no
/// [`META_RESP_CONTENT_TYPE`] entry.
pub const DEFAULT_RESPONSE_CONTENT_TYPE: &str = "application/json";

// ---------------------------------------------------------------------------
// Method → action wire contract
// ---------------------------------------------------------------------------

/// Map a recognized HTTP method token to its [`RequestAction`] wire name,
/// or `None` if the token is not one of the seven mapped methods.
///
/// Case-insensitive. The table is the single wire contract:
/// `GET`/`HEAD` → `retrieve`, `POST` → `create`, `PUT`/`PATCH` → `update`,
/// `DELETE` → `delete`, `OPTIONS` → `execute`.
///
/// `None` lets callers with a mixed vocabulary (e.g. the router's config
/// tokens, which accept canonical action names alongside HTTP methods)
/// distinguish "not an HTTP method" from the [`action_for_http_method`]
/// catch-all.
pub fn try_action_for_http_method(token: &str) -> Option<&'static str> {
    match token.to_ascii_uppercase().as_str() {
        "GET" | "HEAD" => Some(RequestAction::RETRIEVE),
        "POST" => Some(RequestAction::CREATE),
        "PUT" | "PATCH" => Some(RequestAction::UPDATE),
        "DELETE" => Some(RequestAction::DELETE),
        "OPTIONS" => Some(RequestAction::EXECUTE),
        _ => None,
    }
}

/// Map any HTTP method to its [`RequestAction`] wire name.
///
/// Same table as [`try_action_for_http_method`]; methods outside the mapped
/// set (`TRACE`, `CONNECT`, extension methods) fall through to `execute`.
pub fn action_for_http_method(method: &str) -> &'static str {
    try_action_for_http_method(method).unwrap_or(RequestAction::EXECUTE)
}

// ---------------------------------------------------------------------------
// Request head → Message
// ---------------------------------------------------------------------------

/// Build the canonical WAFER [`Message`] for an HTTP request head.
///
/// Platform-neutral: callers adapt their header type to an iterator of
/// `(name, value)` pairs (axum `HeaderMap`, `worker::Headers`, and
/// `web_sys::Headers` all iterate this way). The body is **not** placed on
/// the message — it flows separately via an input stream.
///
/// Produces, in order:
/// - `kind` = `{METHOD}:{path}` (method uppercased);
/// - `http.*` transport meta ([`META_HTTP_METHOD`], [`META_HTTP_PATH`],
///   [`META_HTTP_RAW_QUERY`], [`META_HTTP_REMOTE_ADDR`],
///   [`META_HTTP_CONTENT_TYPE`], [`META_HTTP_HOST`]);
/// - normalized request meta ([`META_REQ_ACTION`] via
///   [`action_for_http_method`], [`META_REQ_RESOURCE`],
///   [`META_REQ_CLIENT_IP`], [`META_REQ_CONTENT_TYPE`]) so downstream blocks
///   need not know the request originated over HTTP;
/// - each header as [`META_HTTP_HEADER_PREFIX`]`{lowercased-name}`;
/// - each query parameter decoded via `url::form_urlencoded` (`+` → space,
///   `%XX`, invalid sequences passed through) into **both**
///   [`META_HTTP_QUERY_PREFIX`]`{key}` and [`META_REQ_QUERY_PREFIX`]`{key}`.
///
/// `raw_query` is the query string without the leading `?` (empty when the
/// URL has none). For `http.content_type`/`http.host` the **first**
/// occurrence of the header wins (matching map-style `get` semantics);
/// repeated headers otherwise follow [`Message::set_meta`] replace-by-key
/// semantics.
pub fn build_http_message<I, N, V>(
    method: &str,
    path: &str,
    raw_query: &str,
    remote_addr: &str,
    headers: I,
) -> Message
where
    I: IntoIterator<Item = (N, V)>,
    N: AsRef<str>,
    V: AsRef<str>,
{
    let method = method.to_ascii_uppercase();
    let mut msg = Message::new(format!("{method}:{path}"));

    // Single pass over the caller's headers: lowercase names, capture
    // content-type/host (first occurrence wins, like a map lookup).
    let mut header_meta: Vec<(String, String)> = Vec::new();
    let mut content_type = String::new();
    let mut host = String::new();
    for (name, value) in headers {
        let name = name.as_ref().to_lowercase();
        let value = value.as_ref();
        if content_type.is_empty() && name == "content-type" {
            content_type = value.to_string();
        }
        if host.is_empty() && name == "host" {
            host = value.to_string();
        }
        header_meta.push((name, value.to_string()));
    }

    // HTTP-specific meta.
    msg.set_meta(META_HTTP_METHOD, &method);
    msg.set_meta(META_HTTP_PATH, path);
    msg.set_meta(META_HTTP_RAW_QUERY, raw_query);
    msg.set_meta(META_HTTP_REMOTE_ADDR, remote_addr);
    msg.set_meta(META_HTTP_CONTENT_TYPE, &content_type);
    msg.set_meta(META_HTTP_HOST, host);

    // Normalized request meta.
    msg.set_meta(META_REQ_ACTION, action_for_http_method(&method));
    msg.set_meta(META_REQ_RESOURCE, path);
    msg.set_meta(META_REQ_CLIENT_IP, remote_addr);
    msg.set_meta(META_REQ_CONTENT_TYPE, content_type);

    for (name, value) in header_meta {
        msg.set_meta(format!("{META_HTTP_HEADER_PREFIX}{name}"), value);
    }

    // Decoded query params (keys AND values run through form_urlencoded).
    if !raw_query.is_empty() {
        for (key, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
            msg.set_meta(format!("{META_HTTP_QUERY_PREFIX}{key}"), value.clone());
            msg.set_meta(format!("{META_REQ_QUERY_PREFIX}{key}"), value);
        }
    }

    msg
}

// ---------------------------------------------------------------------------
// Response meta classification
// ---------------------------------------------------------------------------

/// A typed view of one response-relevant [`MetaEntry`], produced by
/// [`classify_response_meta`]. Adapters apply each part to their platform
/// header/status type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseMetaPart<'a> {
    /// [`META_RESP_STATUS`] with a valid HTTP status code (`100..=999`).
    Status(u16),
    /// [`META_RESP_HEADER_PREFIX`]`{name}` — a response header.
    Header {
        /// Header name as written after the prefix (case preserved).
        name: &'a str,
        /// Header value.
        value: &'a str,
    },
    /// [`META_RESP_COOKIE_PREFIX`]`*` — one `Set-Cookie` directive.
    /// `Set-Cookie` is the one header that must not be joined/replaced, so
    /// it is distinguished from [`Self::Header`]; adapters append it.
    SetCookie(&'a str),
    /// [`META_RESP_CONTENT_TYPE`] — the response `Content-Type`.
    ContentType(&'a str),
}

/// Classify a single meta entry as a response part, or `None` if the key is
/// not response-relevant (including all legacy alias keys — see the module
/// docs) or carries an unusable value (non-numeric / out-of-range
/// [`META_RESP_STATUS`]).
///
/// Per-entry so streaming consumers can classify `Meta` events as they
/// arrive and apply headers before the body finishes — no collection
/// required.
pub fn classify_response_meta(entry: &MetaEntry) -> Option<ResponseMetaPart<'_>> {
    let k = entry.key.as_str();
    let v = entry.value.as_str();
    if k == META_RESP_STATUS {
        return v
            .parse::<u16>()
            .ok()
            .filter(|code| (100..=999).contains(code))
            .map(ResponseMetaPart::Status);
    }
    if k.starts_with(META_RESP_COOKIE_PREFIX) {
        // The key suffix names the cookie (for replace-by-key meta
        // semantics); the directive itself is the value.
        return Some(ResponseMetaPart::SetCookie(v));
    }
    if let Some(name) = k.strip_prefix(META_RESP_HEADER_PREFIX) {
        return Some(ResponseMetaPart::Header { name, value: v });
    }
    if k == META_RESP_CONTENT_TYPE {
        return Some(ResponseMetaPart::ContentType(v));
    }
    None
}

/// Iterate the response parts of a meta slice (entries that don't classify
/// are skipped). Buffered-consumer convenience over
/// [`classify_response_meta`].
pub fn response_meta_parts(meta: &[MetaEntry]) -> impl Iterator<Item = ResponseMetaPart<'_>> {
    meta.iter().filter_map(classify_response_meta)
}

// ---------------------------------------------------------------------------
// Status resolution
// ---------------------------------------------------------------------------

/// Map a semantic [`ErrorCode`] to its canonical HTTP status code.
pub fn error_code_to_http_status(code: &ErrorCode) -> u16 {
    match code {
        ErrorCode::Ok => 200,
        ErrorCode::Cancelled => 499,
        ErrorCode::InvalidArgument | ErrorCode::OutOfRange => 400,
        ErrorCode::DeadlineExceeded => 504,
        ErrorCode::NotFound => 404,
        ErrorCode::AlreadyExists | ErrorCode::Aborted => 409,
        ErrorCode::PermissionDenied => 403,
        ErrorCode::ResourceExhausted => 429,
        ErrorCode::FailedPrecondition => 412,
        ErrorCode::Unimplemented => 501,
        ErrorCode::Unavailable => 503,
        ErrorCode::Unauthenticated => 401,
        ErrorCode::Unknown | ErrorCode::Internal | ErrorCode::DataLoss => 500,
    }
}

/// Resolve the response status from meta: an explicit, valid
/// [`META_RESP_STATUS`] override wins, otherwise `default`. Non-numeric or
/// out-of-range (`< 100`, `> 999`) overrides are ignored.
pub fn resolve_status(meta: &[MetaEntry], default: u16) -> u16 {
    MetaGet::get(meta, META_RESP_STATUS)
        .and_then(|code| code.parse::<u16>().ok())
        .filter(|code| (100..=999).contains(code))
        .unwrap_or(default)
}

/// Resolve the status for an error response: explicit [`META_RESP_STATUS`]
/// override on the error's meta wins, then the status derived from the
/// error's [`ErrorCode`] via [`error_code_to_http_status`].
pub fn resolve_error_status(err: &WaferError) -> u16 {
    resolve_status(&err.meta, error_code_to_http_status(&err.code))
}

// ---------------------------------------------------------------------------
// Buffered terminal mapping
// ---------------------------------------------------------------------------

/// Transport-neutral description of a complete HTTP response.
///
/// Thin platform glue turns this into the platform response type: set the
/// status, **append** each header pair in order (`headers` may legitimately
/// repeat a name — `Set-Cookie` in particular), write the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponseParts {
    /// HTTP status code.
    pub status: u16,
    /// Header pairs in application order, `Set-Cookie` and `Content-Type`
    /// included.
    pub headers: Vec<(String, String)>,
    /// Response body bytes.
    pub body: Vec<u8>,
}

/// Map a successful (or halted) [`BufferedResponse`] to response parts:
/// status from [`resolve_status`] (default `200`), headers from
/// [`response_meta_parts`], defaulting `Content-Type` to
/// [`DEFAULT_RESPONSE_CONTENT_TYPE`] when meta carries none.
///
/// This is the **single** Ok/Halt code path: at the HTTP boundary a `Halt`
/// terminal serves its body+meta exactly like `Complete` (the halt signal
/// was for the flow executor, not the HTTP layer).
pub fn buffered_to_http_response(buf: BufferedResponse) -> HttpResponseParts {
    let status = resolve_status(&buf.meta, 200);
    let mut headers = headers_from_meta(&buf.meta);
    if !MetaGet::contains_key(buf.meta.as_slice(), META_RESP_CONTENT_TYPE) {
        headers.push((
            "Content-Type".to_string(),
            DEFAULT_RESPONSE_CONTENT_TYPE.to_string(),
        ));
    }
    HttpResponseParts {
        status,
        headers,
        body: buf.body,
    }
}

/// Collect a WAFER [`OutputStream`] and map its terminal event to a
/// transport-neutral [`HttpResponseParts`].
///
/// Buffered: the full output body is read into memory before any bytes are
/// produced (streaming consumers classify meta per-event via
/// [`classify_response_meta`] instead). The terminal-event mapping:
///
/// - `Complete` and `Halt` → **identical** handling via
///   [`buffered_to_http_response`] (status override or `200`, meta headers,
///   default `Content-Type: application/json`).
/// - `Error(WaferError)` → status from [`resolve_error_status`], headers
///   from the error's meta, body `{"error": <code>, "message": <msg>}` with
///   `Content-Type: application/json` (the body **is** JSON, so any
///   `resp.content_type` on the error meta is superseded).
/// - `Drop` → `204 No Content`, no headers, empty body.
/// - `Continue` → empty-body `200` with the message's response meta applied
///   and `Content-Type: application/json` (the HTTP boundary has nowhere
///   further to forward).
/// - `Malformed` → `500` with a plain `internal server error` body; logged
///   at `tracing::error` (stream ended without a terminal event — protocol
///   violation).
pub async fn collect_http_response(output: OutputStream) -> HttpResponseParts {
    match output.collect_buffered().await {
        Ok(buf) | Err(TerminalNotResponse::Halt(buf)) => buffered_to_http_response(buf),

        Err(TerminalNotResponse::Error(err)) => {
            let status = resolve_error_status(&err);
            let mut headers = non_content_type_headers_from_meta(&err.meta);
            headers.push((
                "Content-Type".to_string(),
                DEFAULT_RESPONSE_CONTENT_TYPE.to_string(),
            ));
            let body = serde_json::json!({
                "error": err.code,
                "message": err.message,
            })
            .to_string()
            .into_bytes();
            HttpResponseParts {
                status,
                headers,
                body,
            }
        }

        Err(TerminalNotResponse::Drop) => HttpResponseParts {
            status: 204,
            headers: Vec::new(),
            body: Vec::new(),
        },

        Err(TerminalNotResponse::Continue(msg)) => {
            let mut headers = non_content_type_headers_from_meta(&msg.meta);
            headers.push((
                "Content-Type".to_string(),
                DEFAULT_RESPONSE_CONTENT_TYPE.to_string(),
            ));
            HttpResponseParts {
                status: 200,
                headers,
                body: Vec::new(),
            }
        }

        Err(TerminalNotResponse::Malformed) => {
            tracing::error!("HTTP boundary: stream ended without terminal event");
            HttpResponseParts {
                status: 500,
                headers: Vec::new(),
                body: b"internal server error".to_vec(),
            }
        }
    }
}

/// Render classified response meta into header pairs (`Status` parts are
/// resolved separately and skipped here).
fn headers_from_meta(meta: &[MetaEntry]) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    for part in response_meta_parts(meta) {
        match part {
            ResponseMetaPart::Status(_) => {}
            ResponseMetaPart::Header { name, value } => {
                headers.push((name.to_string(), value.to_string()));
            }
            ResponseMetaPart::SetCookie(v) => {
                headers.push(("Set-Cookie".to_string(), v.to_string()));
            }
            ResponseMetaPart::ContentType(v) => {
                headers.push(("Content-Type".to_string(), v.to_string()));
            }
        }
    }
    headers
}

/// Like [`headers_from_meta`] but drops `ContentType` parts — for the
/// Error/Continue arms whose `Content-Type` is fixed to
/// [`DEFAULT_RESPONSE_CONTENT_TYPE`].
fn non_content_type_headers_from_meta(meta: &[MetaEntry]) -> Vec<(String, String)> {
    let mut headers = headers_from_meta(meta);
    headers.retain(|(name, _)| name != "Content-Type");
    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_types::Message;

    fn entry(key: &str, value: &str) -> MetaEntry {
        MetaEntry {
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    // -- method → action wire contract ------------------------------------

    #[test]
    fn method_to_action_table() {
        let table: &[(&str, &str)] = &[
            ("GET", RequestAction::RETRIEVE),
            ("HEAD", RequestAction::RETRIEVE),
            ("POST", RequestAction::CREATE),
            ("PUT", RequestAction::UPDATE),
            ("PATCH", RequestAction::UPDATE),
            ("DELETE", RequestAction::DELETE),
            ("OPTIONS", RequestAction::EXECUTE),
            // Outside the mapped set → execute catch-all.
            ("TRACE", RequestAction::EXECUTE),
            ("CONNECT", RequestAction::EXECUTE),
            ("BREW", RequestAction::EXECUTE),
            // Case-insensitive.
            ("get", RequestAction::RETRIEVE),
            ("Post", RequestAction::CREATE),
        ];
        for (method, action) in table {
            assert_eq!(
                action_for_http_method(method),
                *action,
                "method {method} should map to {action}"
            );
        }
    }

    #[test]
    fn try_action_distinguishes_methods_from_other_tokens() {
        // Mapped HTTP methods → Some.
        assert_eq!(
            try_action_for_http_method("delete"),
            Some(RequestAction::DELETE)
        );
        assert_eq!(
            try_action_for_http_method("OPTIONS"),
            Some(RequestAction::EXECUTE)
        );
        // Canonical action names and arbitrary tokens are NOT methods —
        // callers like the router pass them through their own vocabulary.
        for token in ["retrieve", "create", "list", "TRACE", "BREW", ""] {
            assert_eq!(
                try_action_for_http_method(token),
                None,
                "token {token:?} must not classify as an HTTP method"
            );
        }
    }

    // -- request builder ----------------------------------------------------

    #[test]
    fn build_http_message_produces_canonical_meta() {
        let headers = [
            ("Content-Type", "text/plain"),
            ("Host", "example.com"),
            ("X-Custom-Header", "abc"),
        ];
        let msg = build_http_message("POST", "/things", "a=1&b=hello+world", "1.2.3.4", headers);

        assert_eq!(msg.kind, "POST:/things");
        assert_eq!(msg.get_meta(META_HTTP_METHOD), "POST");
        assert_eq!(msg.get_meta(META_HTTP_PATH), "/things");
        assert_eq!(msg.get_meta(META_HTTP_RAW_QUERY), "a=1&b=hello+world");
        assert_eq!(msg.get_meta(META_HTTP_REMOTE_ADDR), "1.2.3.4");
        assert_eq!(msg.get_meta(META_HTTP_CONTENT_TYPE), "text/plain");
        assert_eq!(msg.get_meta(META_HTTP_HOST), "example.com");

        // Normalized request meta — transport-independent view.
        assert_eq!(msg.get_meta(META_REQ_ACTION), RequestAction::CREATE);
        assert_eq!(msg.get_meta(META_REQ_RESOURCE), "/things");
        assert_eq!(msg.get_meta(META_REQ_CLIENT_IP), "1.2.3.4");
        assert_eq!(msg.get_meta(META_REQ_CONTENT_TYPE), "text/plain");

        // Headers land lowercased under http.header.*.
        assert_eq!(msg.get_meta("http.header.content-type"), "text/plain");
        assert_eq!(msg.get_meta("http.header.x-custom-header"), "abc");

        // Query params land decoded in BOTH http.query.* and req.query.*.
        assert_eq!(msg.get_meta("http.query.a"), "1");
        assert_eq!(msg.get_meta("req.query.a"), "1");
        assert_eq!(msg.get_meta("http.query.b"), "hello world");
        assert_eq!(msg.get_meta("req.query.b"), "hello world");
    }

    #[test]
    fn build_http_message_uppercases_method_and_maps_unknown_to_execute() {
        let msg = build_http_message(
            "brew",
            "/pot",
            "",
            "::1",
            std::iter::empty::<(&str, &str)>(),
        );
        assert_eq!(msg.kind, "BREW:/pot");
        assert_eq!(msg.get_meta(META_HTTP_METHOD), "BREW");
        assert_eq!(msg.get_meta(META_REQ_ACTION), RequestAction::EXECUTE);
        // Absent headers/query → empty-string meta, not missing keys.
        assert_eq!(msg.get_meta(META_HTTP_CONTENT_TYPE), "");
        assert_eq!(msg.get_meta(META_HTTP_HOST), "");
        assert_eq!(msg.get_meta(META_HTTP_RAW_QUERY), "");
    }

    // -- query decoding (pins url::form_urlencoded semantics) ----------------

    #[test]
    fn query_plus_decodes_to_space() {
        let msg = q("q=hello+world");
        assert_eq!(msg.get_meta("req.query.q"), "hello world");
    }

    #[test]
    fn query_percent_xx_decodes() {
        let msg = q("q=hello%20world");
        assert_eq!(msg.get_meta("req.query.q"), "hello world");
    }

    #[test]
    fn query_keys_are_decoded_too() {
        let msg = q("a+b=1&c%2Fd=2");
        assert_eq!(msg.get_meta("req.query.a b"), "1");
        assert_eq!(msg.get_meta("req.query.c/d"), "2");
    }

    #[test]
    fn query_invalid_percent_sequence_is_tolerated() {
        // Invalid %-sequences must not panic; form_urlencoded passes them
        // through verbatim.
        let msg = q("q=hello%ZZworld");
        assert_eq!(msg.get_meta("req.query.q"), "hello%ZZworld");
    }

    #[test]
    fn query_multiple_pairs_round_trip() {
        let msg = q("a=1&b=hello+world&c=%2Fpath");
        for (key, want) in [("a", "1"), ("b", "hello world"), ("c", "/path")] {
            assert_eq!(msg.get_meta(&format!("http.query.{key}")), want);
            assert_eq!(msg.get_meta(&format!("req.query.{key}")), want);
        }
    }

    fn q(raw_query: &str) -> Message {
        build_http_message(
            "GET",
            "/",
            raw_query,
            "::1",
            std::iter::empty::<(&str, &str)>(),
        )
    }

    // -- response meta classification ----------------------------------------

    #[test]
    fn canonical_response_keys_classify() {
        assert_eq!(
            classify_response_meta(&entry(META_RESP_STATUS, "201")),
            Some(ResponseMetaPart::Status(201))
        );
        assert_eq!(
            classify_response_meta(&entry("resp.header.X-Frame-Options", "DENY")),
            Some(ResponseMetaPart::Header {
                name: "X-Frame-Options",
                value: "DENY"
            })
        );
        assert_eq!(
            classify_response_meta(&entry("resp.set_cookie.session", "session=abc; HttpOnly")),
            Some(ResponseMetaPart::SetCookie("session=abc; HttpOnly"))
        );
        assert_eq!(
            classify_response_meta(&entry(META_RESP_CONTENT_TYPE, "text/html")),
            Some(ResponseMetaPart::ContentType("text/html"))
        );
        // Non-response keys are not classified.
        assert_eq!(
            classify_response_meta(&entry("req.action", "retrieve")),
            None
        );
        assert_eq!(classify_response_meta(&entry("trace_id", "t-1")), None);
    }

    /// DRIFT TABLE: the canonical codec honors ONLY the canonical response
    /// meta keys. Every legacy alias that pre-consolidation adapters
    /// (solobase-cloudflare/browser convert.rs, solobase-core pipeline.rs)
    /// tolerated is deliberately ignored — same key, same format,
    /// everywhere. Blocks emitting these keys must move to the canonical
    /// `resp.*` vocabulary.
    #[test]
    fn legacy_keys_are_ignored() {
        let legacy: &[(&str, &str)] = &[
            // Legacy status fallback (cloudflare/browser/pipeline).
            ("http.status", "418"),
            // Legacy cookie prefix (cloudflare doc comment).
            ("resp.cookie.session", "session=abc"),
            // Legacy header/cookie prefixes (cloudflare/browser).
            ("http.resp.header.X-Foo", "bar"),
            ("http.resp.set-cookie.session", "session=abc"),
            // Literal header name as a meta key (cloudflare/browser/pipeline).
            ("Content-Type", "text/html"),
        ];
        for (key, value) in legacy {
            assert_eq!(
                classify_response_meta(&entry(key, value)),
                None,
                "legacy key {key:?} must NOT be honored by the canonical codec"
            );
        }
        // And they don't leak into status resolution either.
        assert_eq!(resolve_status(&[entry("http.status", "418")], 200), 200);
    }

    #[test]
    fn invalid_status_values_are_ignored() {
        for bad in ["", "abc", "42", "1000", "-1", "200.0"] {
            assert_eq!(
                classify_response_meta(&entry(META_RESP_STATUS, bad)),
                None,
                "status value {bad:?} must not classify"
            );
            assert_eq!(
                resolve_status(&[entry(META_RESP_STATUS, bad)], 200),
                200,
                "status value {bad:?} must fall back to the default"
            );
        }
    }

    // -- status resolution ----------------------------------------------------

    /// Pins the full canonical ErrorCode → HTTP status table.
    #[test]
    fn error_code_status_table() {
        let table: &[(ErrorCode, u16)] = &[
            (ErrorCode::Ok, 200),
            (ErrorCode::Cancelled, 499),
            (ErrorCode::Unknown, 500),
            (ErrorCode::InvalidArgument, 400),
            (ErrorCode::DeadlineExceeded, 504),
            (ErrorCode::NotFound, 404),
            (ErrorCode::AlreadyExists, 409),
            (ErrorCode::PermissionDenied, 403),
            (ErrorCode::ResourceExhausted, 429),
            (ErrorCode::FailedPrecondition, 412),
            (ErrorCode::Aborted, 409),
            (ErrorCode::OutOfRange, 400),
            (ErrorCode::Unimplemented, 501),
            (ErrorCode::Internal, 500),
            (ErrorCode::Unavailable, 503),
            (ErrorCode::DataLoss, 500),
            (ErrorCode::Unauthenticated, 401),
        ];
        for (code, status) in table {
            assert_eq!(
                error_code_to_http_status(code),
                *status,
                "{code:?} should map to {status}"
            );
        }
    }

    #[test]
    fn resolve_status_explicit_override_wins() {
        assert_eq!(resolve_status(&[entry(META_RESP_STATUS, "302")], 200), 302);
        assert_eq!(resolve_status(&[], 200), 200);
    }

    #[test]
    fn resolve_error_status_meta_override_beats_code() {
        let mut err = WaferError::new(ErrorCode::NotFound, "missing");
        assert_eq!(resolve_error_status(&err), 404);
        err.meta.push(entry(META_RESP_STATUS, "410"));
        assert_eq!(resolve_error_status(&err), 410);
    }

    // -- buffered terminal mapping ---------------------------------------------

    fn ct(parts: &HttpResponseParts) -> Vec<&str> {
        parts
            .headers
            .iter()
            .filter(|(name, _)| name == "Content-Type")
            .map(|(_, value)| value.as_str())
            .collect()
    }

    #[tokio::test]
    async fn ok_and_halt_are_structurally_identical() {
        let body = b"{\"ok\":true}".to_vec();
        let meta = vec![
            entry(META_RESP_STATUS, "201"),
            entry("resp.header.X-Foo", "bar"),
            entry("resp.set_cookie.session", "session=abc; HttpOnly"),
        ];
        let ok = collect_http_response(OutputStream::respond_with_meta(body.clone(), meta.clone()))
            .await;
        let halt = collect_http_response(OutputStream::halt(body, meta)).await;
        // Finding 55: Ok == Halt at the HTTP boundary, now structurally —
        // both run through buffered_to_http_response.
        assert_eq!(ok, halt);
        assert_eq!(ok.status, 201);
        assert!(ok
            .headers
            .contains(&("X-Foo".to_string(), "bar".to_string())));
        assert!(ok.headers.contains(&(
            "Set-Cookie".to_string(),
            "session=abc; HttpOnly".to_string()
        )));
        // No resp.content_type in meta → default applied.
        assert_eq!(ct(&ok), vec![DEFAULT_RESPONSE_CONTENT_TYPE]);
    }

    #[tokio::test]
    async fn ok_respects_explicit_content_type() {
        let meta = vec![entry(META_RESP_CONTENT_TYPE, "text/html")];
        let parts =
            collect_http_response(OutputStream::respond_with_meta(b"<p>hi</p>".to_vec(), meta))
                .await;
        assert_eq!(parts.status, 200);
        assert_eq!(
            ct(&parts),
            vec!["text/html"],
            "no default when meta sets one"
        );
        assert_eq!(parts.body, b"<p>hi</p>");
    }

    #[tokio::test]
    async fn error_maps_code_to_status_with_json_body() {
        let err = WaferError::new(ErrorCode::NotFound, "no such thing");
        let parts = collect_http_response(OutputStream::error(err)).await;
        assert_eq!(parts.status, 404);
        assert_eq!(ct(&parts), vec![DEFAULT_RESPONSE_CONTENT_TYPE]);
        let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
        assert_eq!(body["error"], "NotFound");
        assert_eq!(body["message"], "no such thing");
    }

    #[tokio::test]
    async fn error_meta_status_override_and_headers_apply() {
        let mut err = WaferError::new(ErrorCode::Internal, "boom");
        err.meta.push(entry(META_RESP_STATUS, "503"));
        err.meta.push(entry("resp.header.Retry-After", "30"));
        // Error bodies ARE JSON: a content-type override on error meta is
        // superseded by application/json (exactly one Content-Type).
        err.meta.push(entry(META_RESP_CONTENT_TYPE, "text/plain"));
        let parts = collect_http_response(OutputStream::error(err)).await;
        assert_eq!(parts.status, 503);
        assert!(parts
            .headers
            .contains(&("Retry-After".to_string(), "30".to_string())));
        assert_eq!(ct(&parts), vec![DEFAULT_RESPONSE_CONTENT_TYPE]);
    }

    #[tokio::test]
    async fn drop_maps_to_204_no_content() {
        let parts = collect_http_response(OutputStream::drop_request()).await;
        assert_eq!(parts.status, 204);
        assert!(parts.headers.is_empty());
        assert!(parts.body.is_empty());
    }

    /// DRIFT DECISION: `Continue` at the HTTP boundary → empty-body `200`
    /// with the message's response meta applied (there is nowhere further
    /// to forward). Pinned per the W2-N canonical-position table.
    #[tokio::test]
    async fn continue_maps_to_empty_200_with_meta_applied() {
        let mut msg = Message::new("next");
        msg.set_meta("resp.header.X-Forwarded-By", "router");
        let parts = collect_http_response(OutputStream::continue_with(msg)).await;
        assert_eq!(parts.status, 200);
        assert!(parts.body.is_empty(), "Continue must produce an empty body");
        assert!(parts
            .headers
            .contains(&("X-Forwarded-By".to_string(), "router".to_string())));
        assert_eq!(ct(&parts), vec![DEFAULT_RESPONSE_CONTENT_TYPE]);
    }

    #[tokio::test]
    async fn malformed_maps_to_500() {
        // A stream that ends without a terminal event. `OutputSink` can no
        // longer produce this (terminal delivery is guaranteed via a reserved
        // channel slot, even when the body channel is full at drop), so the
        // protocol violation is synthesized at the raw channel level — it can
        // still reach consumers from non-sink sources such as a buggy remote
        // producer decoded off the wire.
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tx.try_send(crate::stream::StreamEvent::Chunk(b"partial".to_vec()))
            .unwrap();
        drop(tx); // channel closes with no terminal event
        let stream = OutputStream::from_raw_receiver(rx);
        let parts = collect_http_response(stream).await;
        assert_eq!(parts.status, 500);
        assert_eq!(parts.body, b"internal server error");
    }
}
