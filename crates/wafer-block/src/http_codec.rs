//! Transport-agnostic HTTP ↔ [`Message`] protocol codec.
//!
//! The **single implementation** of the HTTP-to-WAFER protocol mapping. Every
//! HTTP adapter — the native axum listener (`wafer-block-http-listener`), the
//! `wafer-run/router` block's config tokens, and the consuming application's
//! Cloudflare / browser adapters — delegates here instead of carrying its own copy of the
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
//! 5. [`collect_http_response`] / [`buffered_to_http_response`] /
//!    [`error_to_http_response`] — buffered
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
/// URL has none).
///
/// A header the request repeats — any case of its name — becomes **one**
/// entry, its field lines joined in wire order: `Cookie` lines with `"; "`
/// (RFC 6265 §5.4), every other header with `", "` (RFC 9110 §5.3). The
/// mirrors ([`META_HTTP_CONTENT_TYPE`], [`META_HTTP_HOST`],
/// [`META_REQ_CONTENT_TYPE`]) carry the same joined value as their
/// `http.header.*` entry, so no reader sees a different line than another.
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

    // One entry per lowercased name, in first-occurrence order, repeated
    // field lines joined in wire order.
    let mut header_meta: Vec<(String, String)> = Vec::new();
    for (name, value) in headers {
        let name = name.as_ref().to_lowercase();
        let value = value.as_ref();
        match header_meta.iter_mut().find(|(seen, _)| *seen == name) {
            Some((_, joined)) => {
                joined.push_str(if name == "cookie" { "; " } else { ", " });
                joined.push_str(value);
            }
            None => header_meta.push((name, value.to_string())),
        }
    }
    let joined = |wanted: &str| {
        header_meta
            .iter()
            .find(|(name, _)| name == wanted)
            .map_or_else(String::new, |(_, value)| value.clone())
    };
    let content_type = joined("content-type");
    let host = joined("host");

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
    /// [`META_RESP_HEADER_PREFIX`]`{name}` — a response header. `name` is a
    /// valid field name and `value` a valid field value (see
    /// [`classify_response_meta`]).
    Header {
        /// Header name as written after the prefix (case preserved). Never
        /// `Content-Type` or `Set-Cookie` in any case: those classify as
        /// [`Self::ContentType`] and [`Self::SetCookie`].
        name: &'a str,
        /// Header value.
        value: &'a str,
    },
    /// One `Set-Cookie` directive: a [`META_RESP_COOKIE_PREFIX`]`*` entry,
    /// or a [`META_RESP_HEADER_PREFIX`]`{name}` entry whose name is any case
    /// of `set-cookie`. `Set-Cookie` is the one header that must not be
    /// joined/replaced, so it is distinguished from [`Self::Header`];
    /// adapters append it.
    SetCookie(&'a str),
    /// The response `Content-Type`: [`META_RESP_CONTENT_TYPE`], or a
    /// [`META_RESP_HEADER_PREFIX`]`{name}` entry whose name is any case of
    /// `content-type`.
    ContentType(&'a str),
}

/// Response headers the transport owns: connection management
/// (RFC 9110 §7.6.1) and message framing (`Content-Length`,
/// `Transfer-Encoding`). A block's value would contradict the connection or
/// body the adapter actually produces, so these never classify as a
/// [`ResponseMetaPart`].
pub const TRANSPORT_OWNED_RESPONSE_HEADERS: &[&str] = &[
    "connection",
    "content-length",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// A response meta entry [`classify_response_meta`] refuses: the key is
/// response-relevant but its name or value cannot go on the wire. The value
/// is deliberately not carried — it may be a cookie or a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidResponseMeta {
    /// The refused meta key.
    pub key: String,
    /// Why it was refused.
    pub reason: &'static str,
}

impl std::fmt::Display for InvalidResponseMeta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "response meta {:?} refused: {}", self.key, self.reason)
    }
}

impl std::error::Error for InvalidResponseMeta {}

/// An RFC 9110 §5.6.2 token — the grammar of a field name.
fn is_field_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// A field value every transport accepts: visible ASCII, space and
/// horizontal tab. CR, LF, NUL and other controls would split or corrupt the
/// header block; non-ASCII has no portable encoding (a Workers `Headers`
/// refuses code points above U+00FF, hyper sends UTF-8 bytes as opaque
/// `obs-text`), so a producer percent-encodes it (RFC 8187) instead.
fn is_field_value(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b == b'\t' || (b' '..=b'~').contains(&b))
}

/// Classify a single meta entry as a response part.
///
/// - `Ok(None)`: the key is not response-relevant (including all legacy
///   alias keys — see the module docs).
/// - `Ok(Some(part))`: a part every transport can emit.
/// - `Err`: a response key whose entry cannot go on the wire — a
///   [`META_RESP_STATUS`] that is not an integer in `100..=999`, a header
///   name that is not an RFC 9110 token, a header, cookie or content-type
///   value holding anything but visible ASCII, space and tab, or a
///   [`TRANSPORT_OWNED_RESPONSE_HEADERS`] name. The entry is dropped from the
///   response rather than failing it; [`response_meta_parts`] and
///   [`response_meta_entries`] log each one.
///
/// Header names compare case-insensitively: any case of `content-type`
/// classifies as [`ResponseMetaPart::ContentType`], any case of
/// `set-cookie` as [`ResponseMetaPart::SetCookie`].
///
/// Per-entry so streaming consumers can classify `Meta` events as they
/// arrive and apply headers before the body finishes — no collection
/// required.
pub fn classify_response_meta(
    entry: &MetaEntry,
) -> Result<Option<ResponseMetaPart<'_>>, InvalidResponseMeta> {
    let k = entry.key.as_str();
    let v = entry.value.as_str();
    let invalid = |reason| InvalidResponseMeta {
        key: entry.key.clone(),
        reason,
    };
    if k == META_RESP_STATUS {
        return v
            .parse::<u16>()
            .ok()
            .filter(|code| (100..=999).contains(code))
            .map(|code| Some(ResponseMetaPart::Status(code)))
            .ok_or_else(|| invalid("status is not an integer in 100..=999"));
    }
    let part = if k.starts_with(META_RESP_COOKIE_PREFIX) {
        // The key suffix identifies the cookie (see `cookie_meta_key`);
        // the directive itself is the value.
        ResponseMetaPart::SetCookie(v)
    } else if let Some(name) = k.strip_prefix(META_RESP_HEADER_PREFIX) {
        if !is_field_name(name) {
            return Err(invalid("header name is not an RFC 9110 token"));
        }
        if is_listed(TRANSPORT_OWNED_RESPONSE_HEADERS, name) {
            return Err(invalid("header is owned by the transport"));
        }
        if name.eq_ignore_ascii_case("content-type") {
            ResponseMetaPart::ContentType(v)
        } else if name.eq_ignore_ascii_case("set-cookie") {
            ResponseMetaPart::SetCookie(v)
        } else {
            ResponseMetaPart::Header { name, value: v }
        }
    } else if k == META_RESP_CONTENT_TYPE {
        ResponseMetaPart::ContentType(v)
    } else {
        return Ok(None);
    };
    if !is_field_value(v) {
        return Err(invalid("value holds a control or non-ASCII character"));
    }
    Ok(Some(part))
}

fn is_listed(list: &[&str], name: &str) -> bool {
    list.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// The classified response entries of `meta`, in order; refused entries
/// ([`classify_response_meta`] `Err`) are logged at `warn` and skipped.
fn classified(meta: &[MetaEntry]) -> impl Iterator<Item = (&MetaEntry, ResponseMetaPart<'_>)> {
    meta.iter()
        .filter_map(|entry| match classify_response_meta(entry) {
            Ok(part) => part.map(|part| (entry, part)),
            Err(invalid) => {
                tracing::warn!(
                    key = %invalid.key,
                    reason = invalid.reason,
                    "response meta entry dropped at the HTTP boundary"
                );
                None
            }
        })
}

/// Iterate the response parts of a meta slice: entries that are not
/// response-relevant are skipped, refused ones logged and skipped.
/// Buffered-consumer convenience over [`classify_response_meta`].
///
/// The parts are in meta order and may name one header more than once (two
/// cases of a name, a [`META_RESP_CONTENT_TYPE`] beside a
/// `resp.header.content-type`). An adapter keeps one header per
/// case-insensitive name, the later part winning, and appends every
/// [`ResponseMetaPart::SetCookie`] — what [`collect_http_response`] does.
pub fn response_meta_parts(meta: &[MetaEntry]) -> impl Iterator<Item = ResponseMetaPart<'_>> {
    classified(meta).map(|(_, part)| part)
}

/// The response-meta **projection**: the entries of a terminal's meta that
/// may cross a transport boundary, in order, keys and values unchanged.
///
/// Exactly the entries [`classify_response_meta`] accepts — the canonical
/// [`META_RESP_STATUS`], [`META_RESP_HEADER_PREFIX`]`*`,
/// [`META_RESP_COOKIE_PREFIX`]`*` and [`META_RESP_CONTENT_TYPE`] keys, minus
/// the refused ones (logged, as by [`response_meta_parts`]).
/// Everything else on a terminal is request or in-flight state
/// (`http.header.authorization`, `http.header.cookie`, `auth.user_email`,
/// `req.client.ip`, `req.query.*`, …): blocks legitimately carry it — a
/// `Halt` built from the request message is what keeps the CORS and
/// security headers a middleware set on that message — but it is not part of
/// the response and must not leave the runtime.
///
/// Adapters that build a platform response classify with
/// [`response_meta_parts`] instead. This is for boundaries that re-emit meta
/// **as meta**, where the host applies the entries itself: the embedder wire
/// format (`wafer_run::embed::output_to_json`) hands its host a `meta`
/// object. Both projections admit the same key set, so no transport sees
/// more than another.
pub fn response_meta_entries(meta: &[MetaEntry]) -> impl Iterator<Item = &MetaEntry> {
    classified(meta).map(|(entry, _)| entry)
}

// ---------------------------------------------------------------------------
// Cookie identity
// ---------------------------------------------------------------------------

/// A `Set-Cookie` directive's identity: two directives with equal ids set
/// the same browser cookie (RFC 6265 §5.3 step 11), so the later one
/// replaces the earlier.
///
/// `name` and `path` compare exactly (both are case-sensitive, RFC 6265
/// §5.1.4), `domain` case-insensitively and without a leading dot (stored
/// lowercased, dot stripped). A directive without `Path` is NOT the same
/// cookie as one with `Path=/`: the browser gives it the request URI's
/// default path (§5.1.4), which the codec cannot know, so the two are kept
/// apart rather than guessed equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CookieId {
    /// The cookie name (text before the first `=`, trimmed).
    pub name: String,
    /// The `Path` attribute's value, `""` when absent.
    pub path: String,
    /// The `Domain` attribute's value, lowercased without a leading dot,
    /// `""` when absent.
    pub domain: String,
}

/// The [`CookieId`] of a `Set-Cookie` directive.
pub fn cookie_id(directive: &str) -> CookieId {
    let mut parts = directive.split(';');
    let name = parts
        .next()
        .and_then(|pair| pair.split('=').next())
        .unwrap_or("")
        .trim()
        .to_string();
    let mut path = String::new();
    let mut domain = String::new();
    for attr in parts {
        let (key, value) = attr.split_once('=').unwrap_or((attr, ""));
        let value = value.trim();
        if key.trim().eq_ignore_ascii_case("path") {
            path = value.to_string();
        } else if key.trim().eq_ignore_ascii_case("domain") {
            domain = value.trim_start_matches('.').to_ascii_lowercase();
        }
    }
    CookieId { name, path, domain }
}

/// The meta key a `Set-Cookie` directive is stored under: the
/// [`META_RESP_COOKIE_PREFIX`] followed by its [`CookieId`] —
/// `resp.set_cookie.{name}`, then `;Domain={domain}` and `;Path={path}` for
/// the attributes the directive sets (`resp.set_cookie.sid;Path=/api`).
///
/// One key per cookie, whoever writes it: replace-by-key meta semantics
/// ([`Message::set_meta`], the flow executor's merge) then replace a cookie
/// with a later directive for the same cookie, and never a different one.
pub fn cookie_meta_key(directive: &str) -> String {
    let CookieId { name, path, domain } = cookie_id(directive);
    let mut key = format!("{META_RESP_COOKIE_PREFIX}{name}");
    if !domain.is_empty() {
        key.push_str(";Domain=");
        key.push_str(&domain);
    }
    if !path.is_empty() {
        key.push_str(";Path=");
        key.push_str(&path);
    }
    key
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
/// [`response_meta_parts`] (one per case-insensitive name, the later part
/// winning; every `Set-Cookie`), defaulting `Content-Type` to
/// [`DEFAULT_RESPONSE_CONTENT_TYPE`] when meta carries no valid one.
///
/// This is the **single** Ok/Halt code path: at the HTTP boundary a `Halt`
/// terminal serves its body+meta exactly like `Complete` (the halt signal
/// was for the flow executor, not the HTTP layer).
pub fn buffered_to_http_response(buf: BufferedResponse) -> HttpResponseParts {
    let status = resolve_status(&buf.meta, 200);
    let mut headers = headers_from_meta(&buf.meta);
    if !headers.iter().any(|(name, _)| name == "Content-Type") {
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

/// Map a [`WaferError`] terminal to [`HttpResponseParts`].
///
/// This is the **single** Error code path: status from
/// [`resolve_error_status`], headers from the error's meta, and a JSON body
/// `{"error": <ErrorCode>, "message": <msg>, "code": <detail code>}` with
/// `Content-Type: application/json` (the body **is** JSON, so any
/// `resp.content_type` on the error meta is superseded). `code` is the
/// application-level code set via [`WaferError::with_detail_code`] and is
/// omitted when none was attached. Adapters that cannot hand the codec an
/// [`OutputStream`] call this directly so every transport emits the same
/// error body.
pub fn error_to_http_response(err: &WaferError) -> HttpResponseParts {
    let status = resolve_error_status(err);
    let mut headers = non_content_type_headers_from_meta(&err.meta);
    headers.push((
        "Content-Type".to_string(),
        DEFAULT_RESPONSE_CONTENT_TYPE.to_string(),
    ));
    let mut body = serde_json::json!({
        "error": err.code,
        "message": err.message,
    });
    if let Some(detail) = err.detail_code() {
        body["code"] = serde_json::Value::String(detail.to_string());
    }
    HttpResponseParts {
        status,
        headers,
        body: body.to_string().into_bytes(),
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
/// - `Error(WaferError)` → [`error_to_http_response`].
/// - `Drop` → `204 No Content`, empty body, the headers and cookies of the
///   drop's meta (no `Content-Type`: there is no body; a `resp.status` on
///   the meta is ignored — a drop is a 204).
/// - `Continue` → empty-body `200` with the message's response meta applied
///   and `Content-Type: application/json` (the HTTP boundary has nowhere
///   further to forward).
/// - `Malformed` → `500` with a plain `internal server error` body; logged
///   at `tracing::error` (stream ended without a terminal event — protocol
///   violation).
pub async fn collect_http_response(output: OutputStream) -> HttpResponseParts {
    match output.collect_buffered().await {
        Ok(buf) | Err(TerminalNotResponse::Halt(buf)) => buffered_to_http_response(buf),

        Err(TerminalNotResponse::Error(err)) => error_to_http_response(&err),

        Err(TerminalNotResponse::Drop { meta }) => HttpResponseParts {
            status: 204,
            headers: non_content_type_headers_from_meta(&meta),
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
/// resolved separately and skipped here). One pair per case-insensitive
/// name, at its first part's position with its last part's value — the
/// later write wins, as it does across a flow's steps — except
/// `Set-Cookie`, one pair per directive. A content type renders as
/// `Content-Type` whichever key carried it.
fn headers_from_meta(meta: &[MetaEntry]) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = Vec::new();
    for part in response_meta_parts(meta) {
        let (name, value) = match part {
            ResponseMetaPart::Status(_) => continue,
            ResponseMetaPart::SetCookie(v) => {
                headers.push(("Set-Cookie".to_string(), v.to_string()));
                continue;
            }
            ResponseMetaPart::ContentType(v) => ("Content-Type", v),
            ResponseMetaPart::Header { name, value } => (name, value),
        };
        match headers
            .iter_mut()
            .find(|(seen, _)| seen.eq_ignore_ascii_case(name))
        {
            Some(pair) => *pair = (name.to_string(), value.to_string()),
            None => headers.push((name.to_string(), value.to_string())),
        }
    }
    headers
}

/// Like [`headers_from_meta`] but drops `ContentType` parts — for the
/// Error/Continue arms whose `Content-Type` is fixed to
/// [`DEFAULT_RESPONSE_CONTENT_TYPE`], and the bodiless Drop arm.
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

    /// A header the request repeats — in any case — is one entry, its lines
    /// joined in wire order, and every mirror carries that same value.
    #[test]
    fn repeated_request_headers_join_and_mirrors_agree() {
        let headers = [
            ("Content-Type", "text/plain"),
            ("Cookie", "a=1"),
            ("X-Forwarded-For", "203.0.113.99"),
            ("content-type", "application/json"),
            ("cookie", "b=2"),
            ("x-forwarded-for", "198.51.100.7"),
            ("Host", "a.example"),
        ];
        let msg = build_http_message("POST", "/", "", "127.0.0.1", headers);

        let joined_ct = "text/plain, application/json";
        assert_eq!(msg.header("content-type"), joined_ct);
        assert_eq!(msg.get_meta(META_REQ_CONTENT_TYPE), joined_ct);
        assert_eq!(msg.get_meta(META_HTTP_CONTENT_TYPE), joined_ct);
        assert_eq!(msg.header("cookie"), "a=1; b=2");
        assert_eq!(msg.cookie("a"), "1");
        assert_eq!(msg.cookie("b"), "2");
        assert_eq!(msg.header("x-forwarded-for"), "203.0.113.99, 198.51.100.7");
        assert_eq!(msg.get_meta(META_HTTP_HOST), "a.example");
        for name in ["content-type", "cookie", "x-forwarded-for", "host"] {
            let key = format!("{META_HTTP_HEADER_PREFIX}{name}");
            assert_eq!(
                msg.meta.iter().filter(|e| e.key == key).count(),
                1,
                "{key} must be one entry"
            );
        }
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
            Ok(Some(ResponseMetaPart::Status(201)))
        );
        assert_eq!(
            classify_response_meta(&entry("resp.header.X-Frame-Options", "DENY")),
            Ok(Some(ResponseMetaPart::Header {
                name: "X-Frame-Options",
                value: "DENY"
            }))
        );
        assert_eq!(
            classify_response_meta(&entry("resp.set_cookie.session", "session=abc; HttpOnly")),
            Ok(Some(ResponseMetaPart::SetCookie("session=abc; HttpOnly")))
        );
        assert_eq!(
            classify_response_meta(&entry(META_RESP_CONTENT_TYPE, "text/html")),
            Ok(Some(ResponseMetaPart::ContentType("text/html")))
        );
        // Non-response keys are not classified.
        assert_eq!(
            classify_response_meta(&entry("req.action", "retrieve")),
            Ok(None)
        );
        assert_eq!(classify_response_meta(&entry("trace_id", "t-1")), Ok(None));
    }

    /// DRIFT TABLE: the canonical codec honors ONLY the canonical response
    /// meta keys. Every legacy alias that pre-consolidation adapters
    /// (the application's Cloudflare/browser convert.rs, core pipeline.rs)
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
                Ok(None),
                "legacy key {key:?} must NOT be honored by the canonical codec"
            );
        }
        // And they don't leak into status resolution either.
        assert_eq!(resolve_status(&[entry("http.status", "418")], 200), 200);
    }

    #[test]
    fn invalid_status_values_are_ignored() {
        for bad in ["", "abc", "42", "1000", "-1", "200.0"] {
            assert!(
                classify_response_meta(&entry(META_RESP_STATUS, bad)).is_err(),
                "status value {bad:?} must be refused"
            );
            assert_eq!(
                resolve_status(&[entry(META_RESP_STATUS, bad)], 200),
                200,
                "status value {bad:?} must fall back to the default"
            );
        }
    }

    /// Any case of `content-type` is the content type and any case of
    /// `set-cookie` a cookie, never a plain header.
    #[test]
    fn content_type_and_set_cookie_header_names_classify_case_insensitively() {
        for name in ["content-type", "Content-Type", "CONTENT-TYPE"] {
            assert_eq!(
                classify_response_meta(&entry(&format!("resp.header.{name}"), "text/html")),
                Ok(Some(ResponseMetaPart::ContentType("text/html"))),
                "{name}"
            );
        }
        for name in ["set-cookie", "Set-Cookie"] {
            assert_eq!(
                classify_response_meta(&entry(&format!("resp.header.{name}"), "a=1")),
                Ok(Some(ResponseMetaPart::SetCookie("a=1"))),
                "{name}"
            );
        }
    }

    /// An entry that cannot go on the wire is refused, never passed to an
    /// adapter to fail the whole response.
    #[test]
    fn unsendable_response_entries_are_refused() {
        let refused: &[(&str, &str)] = &[
            // Header / cookie / content-type values that would split or
            // corrupt the header block, or have no portable encoding.
            ("resp.header.X-Bad", "a\r\nSet-Cookie: evil=1"),
            ("resp.header.X-Bad", "a\nb"),
            ("resp.header.X-Bad", "a\0b"),
            ("resp.header.X-Bad", "caf\u{e9}"),
            ("resp.set_cookie.sid", "sid=1\r\nX-Evil: 1"),
            (META_RESP_CONTENT_TYPE, "text/html\r\nX-Evil: 1"),
            // Names that are not RFC 9110 tokens.
            ("resp.header.", "v"),
            ("resp.header.X Bad", "v"),
            ("resp.header.X:Bad", "v"),
            ("resp.header.X\r\nBad", "v"),
            // Transport-owned framing and connection headers, any case.
            ("resp.header.Content-Length", "5"),
            ("resp.header.transfer-encoding", "chunked"),
            ("resp.header.Connection", "close"),
            ("resp.header.Upgrade", "websocket"),
        ];
        for (key, value) in refused {
            let entry = entry(key, value);
            let result = classify_response_meta(&entry);
            assert!(
                result.is_err(),
                "{key:?}={value:?} must be refused: {result:?}"
            );
        }
        // The refusal names the key, never the value (it may be a secret).
        let err =
            classify_response_meta(&entry("resp.set_cookie.sid", "sid=s3cret\n")).unwrap_err();
        assert_eq!(err.key, "resp.set_cookie.sid");
        assert!(!err.to_string().contains("s3cret"), "{err}");
    }

    #[test]
    fn cookie_meta_key_is_the_cookie_identity() {
        for (directive, key) in [
            ("sid=1", "resp.set_cookie.sid"),
            ("sid=1; HttpOnly; Secure", "resp.set_cookie.sid"),
            ("sid=1; Path=/api", "resp.set_cookie.sid;Path=/api"),
            (
                "sid=1; path=/; DOMAIN=.A.Example; Max-Age=0",
                "resp.set_cookie.sid;Domain=a.example;Path=/",
            ),
            (" theme = dark", "resp.set_cookie.theme"),
        ] {
            assert_eq!(cookie_meta_key(directive), key, "{directive:?}");
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

    /// A block that sets the content type as a header gets exactly that
    /// content type — not a second, default one beside it.
    #[test]
    fn header_content_type_replaces_the_default() {
        let parts = buffered_to_http_response(BufferedResponse {
            body: b"<p>hi</p>".to_vec(),
            meta: vec![entry("resp.header.content-type", "text/html")],
        });
        assert_eq!(ct(&parts), vec!["text/html"]);
    }

    /// One header per case-insensitive name: the later write wins, at the
    /// earlier one's position; every cookie is kept.
    #[test]
    fn a_header_named_twice_is_one_header_the_later_winning() {
        let parts = buffered_to_http_response(BufferedResponse {
            body: Vec::new(),
            meta: vec![
                entry("resp.header.X-Foo", "first"),
                entry(META_RESP_CONTENT_TYPE, "text/plain"),
                entry("resp.set_cookie.a", "a=1"),
                entry("resp.header.x-foo", "second"),
                entry("resp.header.Content-Type", "text/html"),
                entry("resp.header.Set-Cookie", "b=2"),
            ],
        });
        assert_eq!(
            parts.headers,
            vec![
                ("x-foo".to_string(), "second".to_string()),
                ("Content-Type".to_string(), "text/html".to_string()),
                ("Set-Cookie".to_string(), "a=1".to_string()),
                ("Set-Cookie".to_string(), "b=2".to_string()),
            ]
        );
    }

    /// A refused entry is dropped; the rest of the response stands.
    #[test]
    fn a_refused_entry_does_not_fail_the_response() {
        let parts = buffered_to_http_response(BufferedResponse {
            body: b"ok".to_vec(),
            meta: vec![
                entry(META_RESP_STATUS, "201"),
                entry("resp.header.X-Bad", "a\r\nSet-Cookie: evil=1"),
                entry("resp.header.Content-Length", "999"),
                entry(META_RESP_CONTENT_TYPE, "text/plain\n"),
                entry("resp.header.X-Good", "ok"),
            ],
        });
        assert_eq!(parts.status, 201);
        assert_eq!(
            parts.headers,
            vec![
                ("X-Good".to_string(), "ok".to_string()),
                (
                    "Content-Type".to_string(),
                    DEFAULT_RESPONSE_CONTENT_TYPE.to_string()
                ),
            ]
        );
        assert_eq!(parts.body, b"ok");
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
    async fn error_body_carries_detail_code() {
        let err =
            WaferError::new(ErrorCode::InvalidArgument, "x").with_detail_code("auth.invalid_email");
        let parts = collect_http_response(OutputStream::error(err)).await;
        assert_eq!(parts.status, 400);
        let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
        assert_eq!(body["error"], "InvalidArgument");
        assert_eq!(body["message"], "x");
        assert_eq!(body["code"], "auth.invalid_email");
        // The detail code travels in the body only — it is not a response
        // header.
        assert!(
            parts
                .headers
                .iter()
                .all(|(_, value)| value != "auth.invalid_email"),
            "detail code leaked into headers: {:?}",
            parts.headers
        );
    }

    #[tokio::test]
    async fn error_body_omits_code_without_detail_code() {
        let err = WaferError::new(ErrorCode::InvalidArgument, "x");
        let parts = collect_http_response(OutputStream::error(err)).await;
        let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({ "error": "InvalidArgument", "message": "x" })
        );
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

    /// A drop's meta reaches the 204: its headers and cookies (a
    /// cross-origin caller needs the CORS headers to read even an empty
    /// response), never a `Content-Type` or a status other than 204.
    #[tokio::test]
    async fn drop_with_meta_maps_to_204_carrying_its_headers() {
        let parts = collect_http_response(OutputStream::drop_request_with_meta(vec![
            entry(
                "resp.header.Access-Control-Allow-Origin",
                "https://a.example",
            ),
            entry("resp.set_cookie.sid", "sid=1; Path=/"),
            entry(META_RESP_CONTENT_TYPE, "text/plain"),
            entry(META_RESP_STATUS, "200"),
        ]))
        .await;
        assert_eq!(parts.status, 204);
        assert_eq!(
            parts.headers,
            vec![
                (
                    "Access-Control-Allow-Origin".to_string(),
                    "https://a.example".to_string()
                ),
                ("Set-Cookie".to_string(), "sid=1; Path=/".to_string()),
            ]
        );
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
