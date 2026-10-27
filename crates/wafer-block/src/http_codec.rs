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
//! 5. [`open_response`] — the streaming decision ([`declares_streaming`]):
//!    a response whose leading meta declares it streams is opened as a
//!    [`StreamedHttpResponse`] (status and headers now, the body as a
//!    [`StreamedBody`] as the producer sends it); any other is handed back
//!    for buffering.
//! 6. [`collect_http_response`] / [`terminal_to_http_response`] /
//!    [`buffered_to_http_response`] / [`error_to_http_response`] — buffered
//!    terminal-event mapping from an [`OutputStream`] to a transport-neutral
//!    [`HttpResponseParts`] that thin platform glue turns into an
//!    `axum`/`worker`/`web_sys` response.
//!
//! ## Canonical meta keys
//!
//! Only the canonical response meta keys are honored — those from
//! [`crate::meta`], [`META_RESP_STATUS`], [`META_RESP_HEADER_PREFIX`]`*`,
//! [`META_RESP_COOKIE_PREFIX`]`*`, [`META_RESP_CONTENT_TYPE`], and the two
//! streaming hints that are never headers, [`META_RESP_STREAM`] and
//! [`META_RESP_CONTENT_LENGTH`]. Legacy aliases
//! that pre-consolidation adapters tolerated (`http.status`, `resp.cookie.*`,
//! `http.resp.header.*`, `http.resp.set-cookie.*`, a literal `Content-Type`
//! meta key) are **not** recognized — same key, same format, everywhere. See
//! the `legacy_keys_are_ignored` test for the pinned drift table.

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::stream::Stream;

use crate::{
    core_types::{ErrorCode, Message, MetaEntry, WaferError},
    meta::{
        META_REQ_ACTION, META_REQ_CLIENT_IP, META_REQ_CONTENT_TYPE, META_REQ_QUERY_PREFIX,
        META_REQ_RESOURCE, META_RESP_CONTENT_TYPE, META_RESP_COOKIE_PREFIX,
        META_RESP_HEADER_PREFIX, META_RESP_STATUS,
    },
    stream::StreamEvent,
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
///
/// A header that may appear only once ([`SINGLETON_REQUEST_HEADERS`]) is not
/// a list to join: an adapter that sees individual field lines answers a
/// request repeating one with `400` before building the message (see
/// [`repeated_singleton_header`]; the native listener does).
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

/// Request headers whose value is a single item, not a list (RFC 9110
/// §5.3): two field lines of one of them make the request ambiguous — which
/// `Host` is routed, which `Content-Length` frames the body (RFC 9112 §6.3),
/// which credentials apply — so the request is refused, never joined.
pub const SINGLETON_REQUEST_HEADERS: &[&str] =
    &["authorization", "content-length", "content-type", "host"];

/// The first [`SINGLETON_REQUEST_HEADERS`] name that `names` (a request's
/// field-line names, one per line, any case) holds more than once, or `None`.
/// An adapter that finds one answers `400 Bad Request`.
pub fn repeated_singleton_header<I, N>(names: I) -> Option<&'static str>
where
    I: IntoIterator<Item = N>,
    N: AsRef<str>,
{
    let mut seen = [false; SINGLETON_REQUEST_HEADERS.len()];
    for name in names {
        let name = name.as_ref();
        if let Some(i) = SINGLETON_REQUEST_HEADERS
            .iter()
            .position(|s| s.eq_ignore_ascii_case(name))
        {
            if seen[i] {
                return Some(SINGLETON_REQUEST_HEADERS[i]);
            }
            seen[i] = true;
        }
    }
    None
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
/// [`ResponseMetaPart`]: they are refused as
/// [`InvalidResponseMetaKind::TransportOwned`] and dropped, the response
/// standing without them.
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
/// response-relevant but the entry cannot go on the wire. The value is
/// deliberately not carried — it may be a cookie or a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidResponseMeta {
    /// The refused meta key.
    pub key: String,
    /// Why it was refused.
    pub reason: &'static str,
    /// What the boundary does about it.
    pub kind: InvalidResponseMetaKind,
}

/// How a refused response meta entry affects the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidResponseMetaKind {
    /// A [`TRANSPORT_OWNED_RESPONSE_HEADERS`] header: dropped (logged), the
    /// response stands — the transport sets its own.
    TransportOwned,
    /// A status, header name or value no transport can send. The response
    /// fails closed as [`unsendable_response`]: serving it without the
    /// entry could drop a security header (a `Content-Security-Policy`, a
    /// `Cache-Control: no-store`, a logout's cookie clear) and fail open.
    Unsendable,
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
/// - `Err`: a response key whose entry cannot go on the wire:
///   [`InvalidResponseMetaKind::Unsendable`] for a [`META_RESP_STATUS`] that
///   is not an integer in `100..=999`, a header name that is not an RFC 9110
///   token, or a header, cookie or content-type value holding anything but
///   visible ASCII, space and tab; [`InvalidResponseMetaKind::TransportOwned`]
///   for a [`TRANSPORT_OWNED_RESPONSE_HEADERS`] name.
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
        kind: InvalidResponseMetaKind::Unsendable,
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
            return Err(InvalidResponseMeta {
                kind: InvalidResponseMetaKind::TransportOwned,
                ..invalid("header is owned by the transport")
            });
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

/// The classified response entries of `meta`, in order. A transport-owned
/// entry is logged at `warn` and skipped; the first unsendable one is the
/// `Err`.
fn classified(
    meta: &[MetaEntry],
) -> Result<Vec<(&MetaEntry, ResponseMetaPart<'_>)>, InvalidResponseMeta> {
    let mut parts = Vec::new();
    for entry in meta {
        match classify_response_meta(entry) {
            Ok(Some(part)) => parts.push((entry, part)),
            Ok(None) => {}
            Err(invalid) if invalid.kind == InvalidResponseMetaKind::TransportOwned => {
                tracing::warn!(
                    key = %invalid.key,
                    reason = invalid.reason,
                    "transport-owned response header dropped at the HTTP boundary"
                );
            }
            Err(invalid) => return Err(invalid),
        }
    }
    Ok(parts)
}

/// The response parts of a meta slice, in meta order: entries that are not
/// response-relevant are skipped, transport-owned ones logged and skipped.
/// `Err` is the first unsendable entry: the adapter answers
/// [`unsendable_response`] instead of this response.
///
/// The parts may name one header more than once (two cases of a name, a
/// [`META_RESP_CONTENT_TYPE`] beside a `resp.header.content-type`). An
/// adapter keeps one header per case-insensitive name, the later part
/// winning, and appends every [`ResponseMetaPart::SetCookie`] — what
/// [`collect_http_response`] does.
pub fn response_meta_parts(
    meta: &[MetaEntry],
) -> Result<Vec<ResponseMetaPart<'_>>, InvalidResponseMeta> {
    Ok(classified(meta)?
        .into_iter()
        .map(|(_, part)| part)
        .collect())
}

/// Response headers that describe a body (or a redirect to one): they
/// belong to the body they were set with, so a response that replaces that
/// body — a flow's short-circuit terminal, the 500 answering an unsendable
/// terminal — never carries them. A content type (any case, either key) and
/// `Content-Length` are not listed: neither classifies as a
/// [`ResponseMetaPart::Header`] (the first is the
/// [`ResponseMetaPart::ContentType`], the second is transport-owned).
pub const BODY_RESPONSE_HEADERS: &[&str] = &[
    "content-encoding",
    "content-disposition",
    "content-language",
    "content-location",
    "content-range",
    "accept-ranges",
    "etag",
    "last-modified",
    "location",
];

/// The uniform response for a terminal whose meta holds an unsendable entry
/// (see [`InvalidResponseMetaKind::Unsendable`]): `500` with the
/// [`error_to_http_response`] body of an [`ErrorCode::Internal`] error and
/// the headers of [`unsendable_response_meta`] — the terminal's
/// `Content-Security-Policy`, `X-Frame-Options`, CORS headers and other
/// headers it can send, plus `Cache-Control: no-store`. Logs `invalid`, the
/// first refused key and its reason, at `error`.
///
/// `meta` is the terminal's whole meta, the slice `invalid` came from.
pub fn unsendable_response(meta: &[MetaEntry], invalid: &InvalidResponseMeta) -> HttpResponseParts {
    tracing::error!(
        key = %invalid.key,
        reason = invalid.reason,
        "HTTP boundary: response meta cannot be sent; answering 500"
    );
    let mut headers = Vec::new();
    for entry in &unsendable_response_meta(meta) {
        if let Ok(Some(ResponseMetaPart::Header { name, value })) = classify_response_meta(entry) {
            put_header(&mut headers, name, value);
        }
    }
    let mut parts = error_to_http_response(&WaferError::new(
        ErrorCode::Internal,
        "internal server error",
    ));
    headers.append(&mut parts.headers);
    parts.headers = headers;
    parts
}

/// The response meta of the 500 answering a terminal whose `meta` holds an
/// unsendable entry. Every boundary answering such a terminal —
/// [`unsendable_response`], the embedder wire format — carries exactly
/// these entries.
///
/// They are the terminal's [`ResponseMetaPart::Header`] entries that can be
/// sent, in order, then `resp.header.Cache-Control: no-store`. Skipped:
/// - the refused entries. A refused entry displaces nothing, so for each
///   header name the 500 carries the last value that can be sent — the
///   middleware's `Content-Security-Policy` stands when a later one is
///   malformed;
/// - the terminal's status and content type (the 500's body is JSON);
/// - its `Set-Cookie` directives: the client is told the request failed, so
///   it takes no cookie from the response that failed;
/// - [`BODY_RESPONSE_HEADERS`]: they describe the body the 500 replaces (a
///   `Content-Encoding` the JSON does not have, a `Content-Disposition`
///   that would save the error as a file);
/// - its `Cache-Control` and `Expires`, replaced by `no-store`: they were
///   the freshness of the response that failed, and an error must not be
///   stored — not by a shared cache, which would serve it to every client,
///   and not by the browser. `no-store` also keeps the protection of a
///   terminal that set it itself.
pub fn unsendable_response_meta(meta: &[MetaEntry]) -> Vec<MetaEntry> {
    let mut kept: Vec<MetaEntry> = meta
        .iter()
        .filter(|entry| match classify_response_meta(entry) {
            Ok(Some(ResponseMetaPart::Header { name, .. })) => {
                !is_listed(BODY_RESPONSE_HEADERS, name)
                    && !name.eq_ignore_ascii_case("cache-control")
                    && !name.eq_ignore_ascii_case("expires")
            }
            _ => false,
        })
        .cloned()
        .collect();
    kept.push(MetaEntry {
        key: format!("{META_RESP_HEADER_PREFIX}Cache-Control"),
        value: "no-store".to_string(),
    });
    kept
}

/// The response-meta **projection**: the entries of a terminal's meta that
/// may cross a transport boundary, in order, keys and values unchanged.
///
/// Exactly the entries [`classify_response_meta`] accepts — the canonical
/// [`META_RESP_STATUS`], [`META_RESP_HEADER_PREFIX`]`*`,
/// [`META_RESP_COOKIE_PREFIX`]`*` and [`META_RESP_CONTENT_TYPE`] keys, minus
/// transport-owned ones (logged); `Err` for an unsendable entry, as from
/// [`response_meta_parts`].
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
pub fn response_meta_entries(meta: &[MetaEntry]) -> Result<Vec<&MetaEntry>, InvalidResponseMeta> {
    Ok(classified(meta)?
        .into_iter()
        .map(|(entry, _)| entry)
        .collect())
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
/// out-of-range (`< 100`, `> 999`) overrides are ignored here; rendering the
/// response ([`response_meta_parts`]) refuses them as unsendable.
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
/// [`DEFAULT_RESPONSE_CONTENT_TYPE`] when meta carries none. Meta holding an
/// unsendable entry yields [`unsendable_response`] instead.
///
/// This is the **single** Ok/Halt code path: at the HTTP boundary a `Halt`
/// terminal serves its body+meta exactly like `Complete` (the halt signal
/// was for the flow executor, not the HTTP layer).
pub fn buffered_to_http_response(buf: BufferedResponse) -> HttpResponseParts {
    let mut headers = match headers_from_meta(&buf.meta) {
        Ok(headers) => headers,
        Err(invalid) => return unsendable_response(&buf.meta, &invalid),
    };
    let status = resolve_status(&buf.meta, 200);
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
/// omitted when none was attached. Meta holding an unsendable entry yields
/// [`unsendable_response`] instead. Adapters that cannot hand the codec an
/// [`OutputStream`] call this directly so every transport emits the same
/// error body.
pub fn error_to_http_response(err: &WaferError) -> HttpResponseParts {
    let mut headers = match non_content_type_headers_from_meta(&err.meta) {
        Ok(headers) => headers,
        Err(invalid) => return unsendable_response(&err.meta, &invalid),
    };
    let status = resolve_error_status(err);
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
/// transport-neutral [`HttpResponseParts`]: the canonical renderer of a
/// **buffered** response.
///
/// Buffered: the full output body is read into memory before any bytes are
/// produced. A transport that streams calls [`open_response`] first and
/// collects only what it answers [`OpenedResponse::Buffered`]. The mapping is
/// [`terminal_to_http_response`]'s.
pub async fn collect_http_response(output: OutputStream) -> HttpResponseParts {
    terminal_to_http_response(output.collect_buffered().await)
}

/// Map a collected terminal — what [`OutputStream::collect_buffered`]
/// returns — to a transport-neutral [`HttpResponseParts`]. For a transport
/// that collects the stream itself (under a byte cap, say) and must render
/// the result exactly as [`collect_http_response`] does.
///
/// The terminal-event mapping:
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
/// - Any arm whose meta holds an unsendable entry →
///   [`unsendable_response`].
/// - `Malformed` → `500` with a plain `internal server error` body; logged
///   at `tracing::error` (stream ended without a terminal event — protocol
///   violation).
pub fn terminal_to_http_response(
    terminal: Result<BufferedResponse, TerminalNotResponse>,
) -> HttpResponseParts {
    match terminal {
        Ok(buf) | Err(TerminalNotResponse::Halt(buf)) => buffered_to_http_response(buf),

        Err(TerminalNotResponse::Error(err)) => error_to_http_response(&err),

        Err(TerminalNotResponse::Drop { meta }) => {
            match non_content_type_headers_from_meta(&meta) {
                Ok(headers) => HttpResponseParts {
                    status: 204,
                    headers,
                    body: Vec::new(),
                },
                Err(invalid) => unsendable_response(&meta, &invalid),
            }
        }

        Err(TerminalNotResponse::Continue(msg)) => {
            let mut headers = match non_content_type_headers_from_meta(&msg.meta) {
                Ok(headers) => headers,
                Err(invalid) => return unsendable_response(&msg.meta, &invalid),
            };
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

// ---------------------------------------------------------------------------
// Streamed responses
// ---------------------------------------------------------------------------

/// Response-meta key a block sets, with the value [`STREAM_MARKER_VALUE`],
/// to have its response streamed whatever its content type.
///
/// For large downloads whose content type (`image/*`, `application/pdf`, …)
/// is not one [`is_streaming_content_type`] matches but which must not be
/// held whole in memory. It is consumed by [`declares_streaming`] and is
/// **never** a header: [`classify_response_meta`] does not recognize the key,
/// so every transport leaves it off the wire.
///
/// It also says the response is a **definite** one — a download whose status
/// is known from its leading meta — rather than an open-ended stream (SSE)
/// that only declares a streaming content type; see [`has_stream_marker`].
pub const META_RESP_STREAM: &str = "resp.stream";

/// The value [`META_RESP_STREAM`] carries to opt into streaming.
pub const STREAM_MARKER_VALUE: &str = "1";

/// Response-meta key a streaming block sets to the exact length of the body
/// it is about to send, in bytes (a decimal `u64`), when it knows it — a
/// download knows its object's size.
///
/// Honored only in the leading meta of a response [`open_response`]
/// streams, where it becomes [`StreamedHttpResponse::content_length`]: a
/// transport frames the body with that `Content-Length` instead of chunked
/// encoding, so a client sees the size (a download's progress bar, a `HEAD`
/// answer). [`open_response`] and the [`StreamedBody`] enforce it, so a
/// transport that sends `Content-Length` and the body's `Ok` items, and
/// aborts on an `Err`, needs no counter of its own — even one that stops
/// polling the body once the length is met (hyper does):
/// - a declared `0` with body bytes, or a first chunk already longer than
///   the length, is answered with the 500 at head time;
/// - a chunk that would overrun the length is an `Err` in its place;
/// - the chunk that completes the length is held until the producer's
///   terminal: on `Complete` it is yielded, and if more body bytes come
///   first it becomes an `Err`, so the overrun is never a clean, truncated
///   response;
/// - a body that ends short of the length ends in an `Err`.
///
/// It is **never** a header: [`classify_response_meta`] does not recognize
/// the key (`Content-Length` itself is transport-owned and refused as a
/// header). A buffered response ignores it — its length is the collected
/// body's. A value that is not a decimal `u64` makes a streamed response
/// [`unsendable_response`].
pub const META_RESP_CONTENT_LENGTH: &str = "resp.content_length";

/// The `Content-Type` a streamed response carries when its meta names none:
/// opaque bytes, not the [`DEFAULT_RESPONSE_CONTENT_TYPE`] a buffered
/// response defaults to.
pub const DEFAULT_STREAMED_CONTENT_TYPE: &str = "application/octet-stream";

/// True for content types whose response streams to the client as it is
/// produced instead of being collected first: Server-Sent Events
/// (`text/event-stream`) and generic byte streams
/// (`application/octet-stream`). ASCII case-insensitive; parameters
/// (`; charset=utf-8`) are allowed.
pub fn is_streaming_content_type(content_type: &str) -> bool {
    let lower = content_type.to_ascii_lowercase();
    lower.starts_with("text/event-stream") || lower.starts_with("application/octet-stream")
}

/// The response content type `meta` declares: the value of its **last**
/// content-type entry ([`META_RESP_CONTENT_TYPE`], or any case of
/// `resp.header.content-type`) — the one a rendered response carries, since
/// the later write wins. Read per entry, whatever the other entries hold,
/// and whether or not the value itself can be sent: the decision must not
/// depend on sendability, or a response with an unsendable entry would be
/// collected whole (an endless SSE stream, never) before its 500 went out.
/// [`open_response`] refuses unsendable leading meta at head time instead.
fn leading_content_type(meta: &[MetaEntry]) -> Option<&str> {
    meta.iter().rev().find_map(|entry| {
        let is_content_type = entry.key == META_RESP_CONTENT_TYPE
            || entry
                .key
                .strip_prefix(META_RESP_HEADER_PREFIX)
                .is_some_and(|name| name.eq_ignore_ascii_case("content-type"));
        is_content_type.then_some(entry.value.as_str())
    })
}

/// True when `leading_meta` carries the [`META_RESP_STREAM`] marker with the
/// value [`STREAM_MARKER_VALUE`].
///
/// The marker tells a **definite** streamed response — a download or share
/// whose final status is already known from its leading meta — from an
/// **open-ended** one (SSE, a chat) that declares only a streaming content
/// type and has no definite status or end. A consumer can use the split to
/// record the first kind as it is opened and leave the second alone, since
/// collecting it to learn its outcome would defeat streaming.
pub fn has_stream_marker(leading_meta: &[MetaEntry]) -> bool {
    MetaGet::get(leading_meta, META_RESP_STREAM) == Some(STREAM_MARKER_VALUE)
}

/// The single streaming decision: a response streams iff its **leading**
/// meta — the `Meta` events before its first body event, see
/// [`OutputStream::take_leading_meta`] — declares it, by the
/// [`META_RESP_STREAM`] marker ([`has_stream_marker`]) or by a content type
/// [`is_streaming_content_type`] matches. The content type read is the last
/// one in the meta — the one the response would carry — whatever the other
/// entries hold: a declaration whose meta cannot be sent still declares, and
/// [`open_response`] answers it with the 500 at once.
///
/// A buffered response ([`OutputStream::respond_with_meta`]) carries its meta
/// on the trailing `Complete`, so it has no leading meta and never matches.
pub fn declares_streaming(leading_meta: &[MetaEntry]) -> bool {
    has_stream_marker(leading_meta)
        || leading_content_type(leading_meta).is_some_and(is_streaming_content_type)
}

/// What [`open_response`] made of an [`OutputStream`].
pub enum OpenedResponse {
    /// The response declared streaming and has a body: serve its status and
    /// headers now and its body as the producer sends it.
    Streamed(StreamedHttpResponse),
    /// Serve the response buffered: [`collect_http_response`] (or
    /// [`terminal_to_http_response`] over a collection of the caller's own)
    /// renders it. The stream is the one [`open_response`] was given, its
    /// leading meta put back, so it collects to exactly what the original
    /// would have — or, when the leading meta of a response that declared
    /// streaming cannot be sent, an error stream that renders as
    /// [`unsendable_response`] (the producer is cancelled).
    Buffered(OutputStream),
}

impl std::fmt::Debug for OpenedResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Streamed(response) => f.debug_tuple("Streamed").field(response).finish(),
            Self::Buffered(_) => f.write_str("Buffered(..)"),
        }
    }
}

/// The head of a streamed response and its body.
///
/// A transport sends `status` and `headers` (appending each pair in order:
/// `Set-Cookie` may repeat), then the `body` items as they come. Once the
/// head is sent nothing can change the status, so an `Err` body item is the
/// only way left to say the body is not whole: the transport aborts the
/// response (a cut connection, a chunked body without its last chunk)
/// instead of ending it cleanly.
#[derive(Debug)]
pub struct StreamedHttpResponse {
    /// HTTP status code: the leading meta's [`META_RESP_STATUS`], or `200`.
    pub status: u16,
    /// Header pairs in application order, rendered from the leading meta as a
    /// buffered response's are, with `Content-Type` defaulting to
    /// [`DEFAULT_STREAMED_CONTENT_TYPE`].
    pub headers: Vec<(String, String)>,
    /// The exact body length from [`META_RESP_CONTENT_LENGTH`], when the
    /// producer declared it. `None`: the length is unknown and the transport
    /// frames the body as it ends (chunked encoding).
    pub content_length: Option<u64>,
    /// The body.
    pub body: StreamedBody,
}

/// The body of a [`StreamedHttpResponse`]: a
/// `Stream<Item = Result<Vec<u8>, WaferError>>` read straight from the
/// producer's [`OutputStream`], so backpressure reaches the producer and
/// dropping the body (a client that went away) cancels it.
///
/// - Each `Chunk` is an `Ok` item. A `Meta` event after the body started,
///   and meta on the `Complete` terminal, arrived too late to be a header:
///   it is dropped and logged at `warn` (a late `Set-Cookie` that never
///   reaches the client is a producer bug someone needs to see).
/// - A `Complete` terminal ends the body.
/// - Every other ending is a final `Err`, because the bytes before it are a
///   truncated prefix: an `Error` terminal; a `Halt`, which carries a whole
///   response of its own; a `Drop` or `Continue`, which carry no body and
///   cannot follow one ([`OutputSink`](crate::streams::output::OutputSink)
///   refuses all three after a chunk, so they come only from a non-sink
///   producer); and a stream that ends with no terminal at all.
/// - With a declared [`StreamedHttpResponse::content_length`], a chunk that
///   would overrun it is an `Err` in its place, and a `Complete` that comes
///   short of it is an `Err`: the transport aborts rather than sending more,
///   or fewer, bytes than its `Content-Length` promised. The chunk that
///   completes the length is held back until the terminal (skipping `Meta`
///   and empty chunks): a transport may never poll again once the length is
///   met, so that chunk is yielded only on `Complete`, and is replaced by an
///   `Err` when more body bytes follow it.
///
/// Nothing follows the final item.
pub struct StreamedBody {
    /// `None` once the body has ended.
    output: Option<OutputStream>,
    /// Bytes still owed under a declared content length.
    remaining: Option<u64>,
    /// The chunk that completed the declared length, held until the terminal
    /// shows nothing follows it.
    held: Option<Vec<u8>>,
}

impl std::fmt::Debug for StreamedBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamedBody")
            .field("ended", &self.output.is_none())
            .finish()
    }
}

impl Stream for StreamedBody {
    type Item = Result<Vec<u8>, WaferError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(output) = this.output.as_mut() else {
            return Poll::Ready(None);
        };
        let ending = loop {
            match futures::ready!(Pin::new(&mut *output).poll_next(cx)) {
                Some(StreamEvent::Chunk(bytes)) => match this.remaining {
                    None => return Poll::Ready(Some(Ok(bytes))),
                    // An empty chunk changes nothing, before or after the
                    // length is met.
                    Some(_) if bytes.is_empty() => {}
                    Some(remaining) => match remaining.checked_sub(bytes.len() as u64) {
                        Some(0) => {
                            this.remaining = Some(0);
                            this.held = Some(bytes);
                        }
                        Some(left) => {
                            this.remaining = Some(left);
                            return Poll::Ready(Some(Ok(bytes)));
                        }
                        None => break Some(overrun_error()),
                    },
                },
                Some(StreamEvent::Meta(entry)) => {
                    tracing::warn!(
                        key = %entry.key,
                        "streamed response: meta arrived after the head was sent; dropped"
                    );
                }
                Some(StreamEvent::Complete { meta }) => {
                    if !meta.is_empty() {
                        tracing::warn!(
                            entries = meta.len(),
                            "streamed response: meta on the Complete terminal arrived after \
                             the head was sent; dropped"
                        );
                    }
                    if let Some(last) = this.held.take() {
                        // Nothing followed the chunk that met the length.
                        this.output = None;
                        return Poll::Ready(Some(Ok(last)));
                    }
                    match this.remaining {
                        Some(short) if short > 0 => {
                            break Some(WaferError::new(
                                ErrorCode::Internal,
                                format!(
                                    "the streamed body ended {short} bytes short of its declared \
                                     content length"
                                ),
                            ))
                        }
                        _ => break None,
                    }
                }
                Some(StreamEvent::Error(err)) => break Some(*err),
                Some(StreamEvent::Halt { .. }) => {
                    break Some(WaferError::new(
                        ErrorCode::Internal,
                        "a Halt terminal cannot follow a streamed body",
                    ))
                }
                Some(StreamEvent::Drop { .. }) => {
                    break Some(WaferError::new(
                        ErrorCode::Internal,
                        "a Drop terminal cannot follow a streamed body",
                    ))
                }
                Some(StreamEvent::Continue(_)) => {
                    break Some(WaferError::new(
                        ErrorCode::Internal,
                        "a Continue terminal cannot follow a streamed body",
                    ))
                }
                None => break Some(WaferError::from(TerminalNotResponse::Malformed)),
            }
        };
        // Ended: drop the stream now (its producer is done, or is cancelled
        // by the drop), so nothing is read after the final item.
        this.output = None;
        Poll::Ready(ending.map(Err))
    }
}

/// Open an [`OutputStream`] as an HTTP response: decide whether it streams,
/// and if so render its head now.
///
/// Reads the stream's leading meta ([`OutputStream::take_leading_meta`]). The
/// response is [`OpenedResponse::Streamed`] when that meta
/// [`declares_streaming`] and the next event is a body `Chunk` — the head is
/// committed only once the producer has its first bytes:
/// - `status`: [`resolve_status`] over the leading meta, default `200`;
/// - `headers`: rendered as a buffered response's are, `Content-Type`
///   defaulting to [`DEFAULT_STREAMED_CONTENT_TYPE`];
/// - `content_length`: [`META_RESP_CONTENT_LENGTH`], if declared (the body
///   enforces it);
/// - `body`: the rest of the stream, the first chunk included.
///
/// Everything else is [`OpenedResponse::Buffered`], with the stream exactly
/// as it was: no streaming declaration (a buffered response has no leading
/// meta), or a declaration followed by a terminal before any body (an empty
/// stream renders as the short buffered response it is).
///
/// A streaming declaration whose leading meta holds an entry no transport can
/// send — or a [`META_RESP_CONTENT_LENGTH`] that is not a decimal `u64` — is
/// answered before any header goes out, as [`OpenedResponse::Buffered`] of an
/// error stream that renders as [`unsendable_response`]; the producer's
/// stream is dropped, which cancels it.
pub async fn open_response(mut output: OutputStream) -> OpenedResponse {
    let meta = output.take_leading_meta().await;
    if !declares_streaming(&meta) || !matches!(output.peek().await, Some(StreamEvent::Chunk(_))) {
        output.prepend(meta.into_iter().map(StreamEvent::Meta));
        return OpenedResponse::Buffered(output);
    }
    let head = headers_from_meta(&meta).and_then(|headers| Ok((headers, content_length(&meta)?)));
    let (mut headers, content_length) = match head {
        Ok(head) => head,
        Err(invalid) => return OpenedResponse::Buffered(unsendable_stream(&meta, &invalid)),
    };
    if let Some(declared) = content_length {
        match check_declared_length(&mut output, declared).await {
            DeclaredLength::Fits => {}
            DeclaredLength::Exceeded(invalid) => {
                return OpenedResponse::Buffered(unsendable_stream(&meta, &invalid))
            }
            DeclaredLength::NoBody => {
                output.prepend(meta.into_iter().map(StreamEvent::Meta));
                return OpenedResponse::Buffered(output);
            }
        }
    }
    if !headers.iter().any(|(name, _)| name == "Content-Type") {
        headers.push((
            "Content-Type".to_string(),
            DEFAULT_STREAMED_CONTENT_TYPE.to_string(),
        ));
    }
    OpenedResponse::Streamed(StreamedHttpResponse {
        status: resolve_status(&meta, 200),
        headers,
        content_length,
        body: StreamedBody {
            output: Some(output),
            remaining: content_length,
            held: None,
        },
    })
}

/// The [`META_RESP_CONTENT_LENGTH`] of `meta`: `Ok(None)` when absent, `Err`
/// (unsendable) when it is not a decimal `u64`.
fn content_length(meta: &[MetaEntry]) -> Result<Option<u64>, InvalidResponseMeta> {
    let Some(value) = MetaGet::get(meta, META_RESP_CONTENT_LENGTH) else {
        return Ok(None);
    };
    // `u64::from_str` takes a leading `+`; a length is digits only.
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(content_length_invalid());
    }
    value
        .parse::<u64>()
        .map(Some)
        .map_err(|_| content_length_invalid())
}

/// What [`check_declared_length`] found before the head.
enum DeclaredLength {
    /// Nothing contradicts the declared length yet; [`StreamedBody`] checks
    /// the rest.
    Fits,
    /// A first chunk already longer than the declared length, or a body byte
    /// under a declared `0`: answered with the 500 at head time.
    Exceeded(InvalidResponseMeta),
    /// A declared `0` whose stream ends in something other than `Complete`
    /// (an `Error`, a `Halt`, no terminal): there is no body to stream, so
    /// the response renders buffered, as any declared stream that ends
    /// before its body does.
    NoBody,
}

/// Check the contradictions between a declared content length and the body
/// that are known before the head is committed. `output` is positioned on
/// its first chunk.
///
/// A declared `0` is checked to the terminal now, because a transport never
/// polls a zero-length body: empty chunks are consumed, `Meta` events are
/// kept in place, and the stream is left on the terminal.
async fn check_declared_length(output: &mut OutputStream, declared: u64) -> DeclaredLength {
    use futures::StreamExt;
    if declared > 0 {
        return match output.peek().await {
            Some(StreamEvent::Chunk(first)) if first.len() as u64 > declared => {
                DeclaredLength::Exceeded(body_exceeds_content_length())
            }
            _ => DeclaredLength::Fits,
        };
    }
    let mut skipped_meta = Vec::new();
    let found = loop {
        match output.peek().await {
            Some(StreamEvent::Chunk(bytes)) if bytes.is_empty() => {
                output.next().await;
            }
            Some(StreamEvent::Meta(_)) => {
                if let Some(event) = output.next().await {
                    skipped_meta.push(event);
                }
            }
            Some(StreamEvent::Chunk(_)) => {
                break DeclaredLength::Exceeded(body_exceeds_content_length())
            }
            Some(StreamEvent::Complete { .. }) => break DeclaredLength::Fits,
            _ => break DeclaredLength::NoBody,
        }
    };
    output.prepend(skipped_meta);
    found
}

fn body_exceeds_content_length() -> InvalidResponseMeta {
    InvalidResponseMeta {
        key: META_RESP_CONTENT_LENGTH.to_string(),
        reason: "the body is longer than its declared content length",
        kind: InvalidResponseMetaKind::Unsendable,
    }
}

fn overrun_error() -> WaferError {
    WaferError::new(
        ErrorCode::Internal,
        "the streamed body overran its declared content length",
    )
}

fn content_length_invalid() -> InvalidResponseMeta {
    InvalidResponseMeta {
        key: META_RESP_CONTENT_LENGTH.to_string(),
        reason: "content length is not a decimal u64",
        kind: InvalidResponseMetaKind::Unsendable,
    }
}

/// A stream [`collect_http_response`] renders exactly as
/// [`unsendable_response`]`(meta, invalid)`: an `Internal` error terminal
/// carrying [`unsendable_response_meta`]. Logs as `unsendable_response` does.
fn unsendable_stream(meta: &[MetaEntry], invalid: &InvalidResponseMeta) -> OutputStream {
    tracing::error!(
        key = %invalid.key,
        reason = invalid.reason,
        "HTTP boundary: response meta cannot be sent; answering 500"
    );
    let mut err = WaferError::new(ErrorCode::Internal, "internal server error");
    err.meta = unsendable_response_meta(meta);
    OutputStream::error(err)
}

/// Render classified response meta into header pairs (`Status` parts are
/// resolved separately and skipped here). One pair per case-insensitive
/// name, at its first part's position with its last part's value — the
/// later write wins, as it does across a flow's steps — except
/// `Set-Cookie`, one pair per directive. A content type renders as
/// `Content-Type` whichever key carried it. `Err` for an unsendable entry.
fn headers_from_meta(meta: &[MetaEntry]) -> Result<Vec<(String, String)>, InvalidResponseMeta> {
    let mut headers: Vec<(String, String)> = Vec::new();
    for part in response_meta_parts(meta)? {
        let (name, value) = match part {
            ResponseMetaPart::Status(_) => continue,
            ResponseMetaPart::SetCookie(v) => {
                headers.push(("Set-Cookie".to_string(), v.to_string()));
                continue;
            }
            ResponseMetaPart::ContentType(v) => ("Content-Type", v),
            ResponseMetaPart::Header { name, value } => (name, value),
        };
        put_header(&mut headers, name, value);
    }
    Ok(headers)
}

/// Set `name` in `headers`: one pair per case-insensitive name, at its first
/// position with the latest value — the later write wins.
fn put_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    match headers
        .iter_mut()
        .find(|(seen, _)| seen.eq_ignore_ascii_case(name))
    {
        Some(pair) => *pair = (name.to_string(), value.to_string()),
        None => headers.push((name.to_string(), value.to_string())),
    }
}

/// Like [`headers_from_meta`] but drops `ContentType` parts — for the
/// Error/Continue arms whose `Content-Type` is fixed to
/// [`DEFAULT_RESPONSE_CONTENT_TYPE`], and the bodiless Drop arm.
fn non_content_type_headers_from_meta(
    meta: &[MetaEntry],
) -> Result<Vec<(String, String)>, InvalidResponseMeta> {
    let mut headers = headers_from_meta(meta)?;
    headers.retain(|(name, _)| name != "Content-Type");
    Ok(headers)
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
        let kind =
            |key: &str, value: &str| classify_response_meta(&entry(key, value)).unwrap_err().kind;
        assert_eq!(
            kind("resp.header.Content-Length", "5"),
            InvalidResponseMetaKind::TransportOwned
        );
        assert_eq!(
            kind("resp.header.X-Bad", "a\nb"),
            InvalidResponseMetaKind::Unsendable
        );
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

    /// A transport-owned header is dropped; the rest of the response stands.
    #[test]
    fn a_transport_owned_header_is_dropped_and_the_response_stands() {
        let parts = buffered_to_http_response(BufferedResponse {
            body: b"ok".to_vec(),
            meta: vec![
                entry(META_RESP_STATUS, "201"),
                entry("resp.header.Content-Length", "999"),
                entry("resp.header.Connection", "close"),
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

    /// The 500 answering unsendable meta: the JSON `Internal` body, `headers`
    /// kept from the terminal, then the JSON `Content-Type`.
    fn assert_uniform_500(parts: &HttpResponseParts, headers: &[(&str, &str)]) {
        assert_eq!(parts.status, 500, "{parts:?}");
        let mut expected: Vec<(String, String)> = headers
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect();
        expected.push((
            "Content-Type".to_string(),
            DEFAULT_RESPONSE_CONTENT_TYPE.to_string(),
        ));
        assert_eq!(parts.headers, expected, "{parts:?}");
        let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({ "error": "Internal", "message": "internal server error" })
        );
    }

    /// An unsendable entry fails the response closed: a CSP with one smart
    /// quote must not ship the page with no CSP at all. The 500 still
    /// carries the terminal's security headers — the middleware's
    /// `X-Frame-Options` and CSP, a CORS header — but not its cookie, status
    /// or content type, and is `no-store`.
    #[tokio::test]
    async fn an_unsendable_entry_fails_every_terminal_closed() {
        let meta = || {
            vec![
                entry("resp.header.X-Frame-Options", "DENY"),
                entry("resp.header.Content-Security-Policy", "default-src 'self'"),
                entry("resp.set_cookie.sid", "sid=abc; Path=/"),
                entry(
                    "resp.header.Access-Control-Allow-Origin",
                    "https://a.example",
                ),
                entry(META_RESP_STATUS, "201"),
                entry(META_RESP_CONTENT_TYPE, "text/html"),
                entry(
                    "resp.header.content-security-policy",
                    "script-src \u{2019}self\u{2019}",
                ),
                entry("resp.header.Content-Length", "3"),
            ]
        };
        let kept = [
            ("X-Frame-Options", "DENY"),
            ("Content-Security-Policy", "default-src 'self'"),
            ("Access-Control-Allow-Origin", "https://a.example"),
            ("Cache-Control", "no-store"),
        ];
        let ok = collect_http_response(OutputStream::respond_with_meta(b"<p>".to_vec(), meta()));
        assert_uniform_500(&ok.await, &kept);
        let halt = collect_http_response(OutputStream::halt(b"<p>".to_vec(), meta()));
        assert_uniform_500(&halt.await, &kept);
        let mut err = WaferError::new(ErrorCode::Unauthenticated, "sign in");
        err.meta = meta();
        assert_uniform_500(
            &collect_http_response(OutputStream::error(err)).await,
            &kept,
        );
        let drop = collect_http_response(OutputStream::drop_request_with_meta(meta()));
        assert_uniform_500(&drop.await, &kept);
        let mut msg = Message::new("next");
        msg.meta = meta();
        assert_uniform_500(
            &collect_http_response(OutputStream::continue_with(msg)).await,
            &kept,
        );
        // An invalid status and an invalid header name fail it the same way.
        for bad in [
            entry(META_RESP_STATUS, "abc"),
            entry("resp.header.X Bad", "v"),
        ] {
            let parts = buffered_to_http_response(BufferedResponse {
                body: Vec::new(),
                meta: vec![entry("resp.header.X-Content-Type-Options", "nosniff"), bad],
            });
            assert_uniform_500(
                &parts,
                &[
                    ("X-Content-Type-Options", "nosniff"),
                    ("Cache-Control", "no-store"),
                ],
            );
        }
    }

    /// The 500 replaces the terminal's body, so it drops the headers that
    /// describe that body and its freshness — a streamed download's
    /// `Content-Encoding`, `Content-Disposition`, validators and caching —
    /// and is `no-store` whatever the terminal said.
    #[test]
    fn the_unsendable_500_drops_body_and_cache_headers() {
        let mut meta = vec![entry("resp.header.X-Frame-Options", "DENY")];
        for name in BODY_RESPONSE_HEADERS {
            meta.push(entry(&format!("resp.header.{name}"), "v"));
        }
        meta.extend([
            entry("resp.header.Content-Encoding", "gzip"),
            entry(
                "resp.header.Content-Disposition",
                "attachment; filename=\"a.zip\"",
            ),
            entry("resp.header.ETag", "\"abc\""),
            entry("resp.header.Cache-Control", "public, max-age=31536000"),
            entry("resp.header.expires", "Thu, 01 Jan 2099 00:00:00 GMT"),
            entry("resp.header.X-Bad", "\u{2019}"),
        ]);
        let parts = buffered_to_http_response(BufferedResponse {
            body: b"PK".to_vec(),
            meta,
        });
        assert_uniform_500(
            &parts,
            &[("X-Frame-Options", "DENY"), ("Cache-Control", "no-store")],
        );
    }

    #[test]
    fn repeated_singleton_request_headers_are_found() {
        assert_eq!(
            repeated_singleton_header(["Host", "Accept", "host"]),
            Some("host")
        );
        assert_eq!(
            repeated_singleton_header(["authorization", "Authorization"]),
            Some("authorization")
        );
        assert_eq!(
            repeated_singleton_header(["Content-Length", "content-length"]),
            Some("content-length")
        );
        assert_eq!(
            repeated_singleton_header(["Content-Type", "CONTENT-TYPE"]),
            Some("content-type")
        );
        assert_eq!(
            repeated_singleton_header([
                "Cookie",
                "cookie",
                "X-Forwarded-For",
                "x-forwarded-for",
                "Host"
            ]),
            None
        );
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

    // -- streamed responses -----------------------------------------------

    /// A stream of exactly `events`, then the end of the channel: a stream
    /// with no terminal when `events` holds none.
    fn events_stream(events: Vec<StreamEvent>) -> OutputStream {
        let (tx, rx) = tokio::sync::mpsc::channel(events.len().max(1));
        for event in events {
            tx.try_send(event).unwrap();
        }
        OutputStream::from_raw_receiver(rx)
    }

    fn sse_meta() -> MetaEntry {
        entry(META_RESP_CONTENT_TYPE, "text/event-stream")
    }

    fn marker() -> MetaEntry {
        entry(META_RESP_STREAM, STREAM_MARKER_VALUE)
    }

    fn chunk(bytes: &[u8]) -> StreamEvent {
        StreamEvent::Chunk(bytes.to_vec())
    }

    fn complete() -> StreamEvent {
        StreamEvent::Complete { meta: vec![] }
    }

    fn streamed(opened: OpenedResponse) -> StreamedHttpResponse {
        match opened {
            OpenedResponse::Streamed(response) => response,
            OpenedResponse::Buffered(_) => panic!("expected a streamed response"),
        }
    }

    fn buffered(opened: OpenedResponse) -> OutputStream {
        match opened {
            OpenedResponse::Buffered(stream) => stream,
            OpenedResponse::Streamed(response) => {
                panic!("expected a buffered response, got {response:?}")
            }
        }
    }

    /// Every item of `body`, bounded so a body that never ends fails the
    /// test instead of hanging it.
    async fn body_items(body: StreamedBody) -> Vec<Result<Vec<u8>, WaferError>> {
        use futures::StreamExt;
        let items: Vec<_> = body.take(64).collect().await;
        assert!(items.len() < 64, "the body never ended: {items:?}");
        items
    }

    /// `open_response` over a stream whose producer is still running: it must
    /// return without waiting for the producer to end.
    async fn open_live(output: OutputStream) -> OpenedResponse {
        tokio::time::timeout(std::time::Duration::from_secs(5), open_response(output))
            .await
            .expect("open_response waited for a producer that is still running")
    }

    fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn streaming_content_types_are_sse_and_octet_stream_only() {
        assert!(is_streaming_content_type("text/event-stream"));
        assert!(is_streaming_content_type("application/octet-stream"));
        assert!(is_streaming_content_type(
            "TEXT/EVENT-STREAM; charset=utf-8"
        ));
        assert!(!is_streaming_content_type("image/png"));
        assert!(!is_streaming_content_type("application/json"));
    }

    #[test]
    fn declares_streaming_on_the_marker_whatever_the_content_type() {
        assert!(declares_streaming(&[
            marker(),
            entry(META_RESP_CONTENT_TYPE, "image/png")
        ]));
        assert!(declares_streaming(&[marker()]));
        assert!(has_stream_marker(&[marker()]));
    }

    #[test]
    fn declares_streaming_on_a_streaming_content_type_under_either_key() {
        assert!(declares_streaming(&[sse_meta()]));
        assert!(declares_streaming(&[entry(
            "resp.header.CONTENT-TYPE",
            "application/octet-stream"
        )]));
        assert!(!has_stream_marker(&[sse_meta()]));
    }

    #[test]
    fn declares_streaming_is_false_for_the_buffered_shape() {
        // A buffered response has no leading meta at all.
        assert!(!declares_streaming(&[]));
        // A content type that is not a streaming one does not stream.
        assert!(!declares_streaming(&[entry(
            META_RESP_CONTENT_TYPE,
            "application/json"
        )]));
        // The marker with another value does not opt in.
        assert!(!declares_streaming(&[entry(META_RESP_STREAM, "0")]));
        assert!(!has_stream_marker(&[entry(META_RESP_STREAM, "true")]));
    }

    /// The decision reads the content type the response would carry — the
    /// last one — whatever the other entries hold, and whether or not the
    /// value itself can be sent.
    #[test]
    fn declares_streaming_reads_the_last_content_type_whatever_else_the_meta_holds() {
        assert!(declares_streaming(&[
            sse_meta(),
            entry("resp.header.X-Bad", "a\nb")
        ]));
        assert!(declares_streaming(&[entry(
            META_RESP_CONTENT_TYPE,
            "text/event-stream\n"
        )]));
        assert!(declares_streaming(&[
            entry("resp.header.Content-Type", "text/html"),
            sse_meta()
        ]));
        assert!(!declares_streaming(&[
            sse_meta(),
            entry("resp.header.Content-Type", "text/html")
        ]));
    }

    #[test]
    fn the_streaming_hints_are_never_headers() {
        let meta = [marker(), entry(META_RESP_CONTENT_LENGTH, "12")];
        assert!(response_meta_parts(&meta).unwrap().is_empty());
        assert!(response_meta_entries(&meta).unwrap().is_empty());
    }

    /// An SSE response opens with its head as soon as it has a first chunk:
    /// status and headers from the leading meta, the body still coming from
    /// the live producer, which is not cancelled.
    #[tokio::test]
    async fn an_sse_response_opens_streamed_before_its_producer_ends() {
        let (stream, sink, cancel) = OutputStream::new_streaming();
        sink.send_meta(sse_meta()).await.unwrap();
        sink.send_meta(entry("resp.header.X-Frame-Options", "DENY"))
            .await
            .unwrap();
        sink.send_chunk(b"data: one\n\n".to_vec()).await.unwrap();

        let response = streamed(open_live(stream).await);
        assert_eq!(response.status, 200);
        assert_eq!(
            response.headers,
            vec![
                ("Content-Type".to_string(), "text/event-stream".to_string()),
                ("X-Frame-Options".to_string(), "DENY".to_string()),
            ]
        );
        assert_eq!(response.content_length, None);
        assert!(
            !cancel.is_cancelled(),
            "an open response keeps its producer"
        );

        sink.send_chunk(b"data: two\n\n".to_vec())
            .await
            .expect("the body still reads from the producer");
        sink.complete(vec![]).await.unwrap();
        let items = body_items(response.body).await;
        let items: Vec<Vec<u8>> = items.into_iter().map(Result::unwrap).collect();
        assert_eq!(
            items,
            vec![b"data: one\n\n".to_vec(), b"data: two\n\n".to_vec()]
        );
    }

    /// Dropping a streamed response — a client that went away — cancels the
    /// producer and closes its channel.
    #[tokio::test]
    async fn dropping_a_streamed_response_cancels_its_producer() {
        let (stream, sink, cancel) = OutputStream::new_streaming();
        sink.send_meta(sse_meta()).await.unwrap();
        sink.send_chunk(b"data: one\n\n".to_vec()).await.unwrap();
        let response = streamed(open_live(stream).await);
        assert!(!cancel.is_cancelled());
        drop(response);
        assert!(cancel.is_cancelled());
        assert!(sink.send_chunk(b"data: two\n\n".to_vec()).await.is_err());
    }

    /// The marker streams a download whatever its content type, which is
    /// sent as is; the marker itself is not a header.
    #[tokio::test]
    async fn a_marker_declared_download_opens_streamed_with_its_own_content_type() {
        let response = streamed(
            open_response(events_stream(vec![
                StreamEvent::Meta(marker()),
                StreamEvent::Meta(entry(META_RESP_CONTENT_TYPE, "image/png")),
                StreamEvent::Meta(entry(
                    "resp.header.Content-Disposition",
                    "attachment; filename=\"a.png\"",
                )),
                StreamEvent::Meta(entry(META_RESP_STATUS, "206")),
                chunk(b"png"),
                complete(),
            ]))
            .await,
        );
        assert_eq!(response.status, 206);
        assert_eq!(
            response.headers,
            vec![
                ("Content-Type".to_string(), "image/png".to_string()),
                (
                    "Content-Disposition".to_string(),
                    "attachment; filename=\"a.png\"".to_string()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn a_streamed_response_without_a_content_type_is_octet_stream() {
        let response = streamed(
            open_response(events_stream(vec![
                StreamEvent::Meta(marker()),
                chunk(b"bytes"),
                complete(),
            ]))
            .await,
        );
        assert_eq!(
            header(&response.headers, "content-type"),
            Some(DEFAULT_STREAMED_CONTENT_TYPE)
        );
    }

    /// The content-length hint becomes the response's length, and is not a
    /// header.
    #[tokio::test]
    async fn a_declared_content_length_is_carried_to_the_response() {
        let response = streamed(
            open_response(events_stream(vec![
                StreamEvent::Meta(marker()),
                StreamEvent::Meta(entry(META_RESP_CONTENT_LENGTH, "5")),
                chunk(b"bytes"),
                complete(),
            ]))
            .await,
        );
        assert_eq!(response.content_length, Some(5));
        assert_eq!(header(&response.headers, "content-length"), None);
        let max = u64::MAX.to_string();
        let response = streamed(
            open_response(events_stream(vec![
                StreamEvent::Meta(marker()),
                StreamEvent::Meta(entry(META_RESP_CONTENT_LENGTH, &max)),
                chunk(b"bytes"),
                complete(),
            ]))
            .await,
        );
        assert_eq!(response.content_length, Some(u64::MAX));
    }

    /// A content length that is not a decimal `u64` fails the response
    /// closed before its head, as an unsendable header does.
    #[tokio::test]
    async fn an_invalid_content_length_is_unsendable() {
        for value in ["", "+5", "-1", "1.0", "0x10", " 5", "18446744073709551616"] {
            let meta = vec![
                marker(),
                entry("resp.header.X-Frame-Options", "DENY"),
                entry(META_RESP_CONTENT_LENGTH, value),
            ];
            let mut events: Vec<StreamEvent> =
                meta.iter().cloned().map(StreamEvent::Meta).collect();
            events.extend([chunk(b"bytes"), complete()]);
            let parts =
                collect_http_response(buffered(open_response(events_stream(events)).await)).await;
            assert_eq!(
                parts,
                unsendable_response(&meta, &content_length_invalid()),
                "{value:?}"
            );
        }
    }

    /// A response whose meta is all trailing — every buffered helper's shape
    /// — is buffered, even when its content type is a streaming one, and
    /// renders exactly as it did before `open_response` read it.
    #[tokio::test]
    async fn a_trailing_streaming_content_type_is_buffered() {
        let make =
            || OutputStream::respond_with_meta(b"data: x\n\n".to_vec(), vec![sse_meta(), marker()]);
        let opened = buffered(open_response(make()).await);
        assert_eq!(
            collect_http_response(opened).await,
            collect_http_response(make()).await
        );
    }

    /// Leading meta that declares nothing is put back: the buffered render is
    /// byte-identical to rendering the original stream.
    #[tokio::test]
    async fn leading_meta_without_a_declaration_is_buffered_unchanged() {
        let make = || {
            events_stream(vec![
                StreamEvent::Meta(entry(META_RESP_CONTENT_TYPE, "text/html")),
                StreamEvent::Meta(entry(META_RESP_STATUS, "201")),
                StreamEvent::Meta(entry("resp.set_cookie.a", "a=1")),
                chunk(b"<p>"),
                StreamEvent::Meta(entry("resp.header.X-Late", "1")),
                chunk(b"hi</p>"),
                StreamEvent::Complete {
                    meta: vec![entry("resp.header.ETag", "\"v1\"")],
                },
            ])
        };
        let opened = buffered(open_response(make()).await);
        let parts = collect_http_response(opened).await;
        assert_eq!(parts, collect_http_response(make()).await);
        assert_eq!(parts.status, 201);
        assert_eq!(parts.body, b"<p>hi</p>");
        assert_eq!(header(&parts.headers, "x-late"), Some("1"));
    }

    /// A stream that declares streaming but ends before any body renders as
    /// the short buffered response it is — whatever terminal it ends with.
    #[tokio::test]
    async fn a_declared_stream_with_no_body_is_buffered() {
        let cases: Vec<Box<dyn Fn() -> OutputStream>> = vec![
            Box::new(|| events_stream(vec![StreamEvent::Meta(sse_meta()), complete()])),
            Box::new(|| {
                events_stream(vec![
                    StreamEvent::Meta(marker()),
                    StreamEvent::Error(Box::new(WaferError::new(ErrorCode::NotFound, "gone"))),
                ])
            }),
            Box::new(|| {
                events_stream(vec![
                    StreamEvent::Meta(sse_meta()),
                    StreamEvent::Meta(entry(META_RESP_STATUS, "202")),
                ])
            }),
        ];
        for make in cases {
            let opened = buffered(open_response(make()).await);
            assert_eq!(
                collect_http_response(opened).await,
                collect_http_response(make()).await
            );
        }
    }

    /// Leading meta no transport can send fails closed before the head, with
    /// a producer that never ends: the 500 `unsendable_response` renders at
    /// once, keeping the sendable security headers and adding `no-store`, and
    /// the producer is cancelled.
    async fn assert_unsendable_500_at_head(leading: Vec<MetaEntry>) {
        let (stream, sink, cancel) = OutputStream::new_streaming();
        for entry in &leading {
            sink.send_meta(entry.clone()).await.unwrap();
        }
        sink.send_chunk(b"data: one\n\n".to_vec()).await.unwrap();
        // `sink` stays alive and never sends a terminal: the producer of an
        // open-ended stream.

        let opened = buffered(open_live(stream).await);
        assert!(
            cancel.is_cancelled(),
            "the producer of a response answered with a 500 is cancelled"
        );
        let invalid = response_meta_parts(&leading).unwrap_err();
        let parts = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            collect_http_response(opened),
        )
        .await
        .expect("the 500 renders without waiting for the producer");
        assert_eq!(parts, unsendable_response(&leading, &invalid));
        assert_eq!(parts.status, 500);
        assert_eq!(header(&parts.headers, "cache-control"), Some("no-store"));
        assert_eq!(
            header(&parts.headers, "content-security-policy"),
            Some("default-src 'none'")
        );
        assert_eq!(header(&parts.headers, "x-bad"), None);
        drop(sink);
    }

    fn unsendable_leading_meta() -> Vec<MetaEntry> {
        vec![
            sse_meta(),
            entry("resp.header.Content-Security-Policy", "default-src 'none'"),
            entry("resp.header.Cache-Control", "max-age=60"),
            entry("resp.header.X-Bad", "a\r\nInjected: 1"),
        ]
    }

    #[tokio::test]
    async fn a_marker_declared_stream_with_unsendable_leading_meta_is_a_500_at_head() {
        let mut leading = unsendable_leading_meta();
        leading.insert(0, marker());
        assert_unsendable_500_at_head(leading).await;
    }

    /// No marker: the SSE content type alone declares the stream, so an
    /// unsendable header beside it is still answered before the head — not
    /// after collecting a body that never ends.
    #[tokio::test]
    async fn an_sse_stream_with_unsendable_leading_meta_is_a_500_at_head() {
        assert_unsendable_500_at_head(unsendable_leading_meta()).await;
    }

    /// An SSE content type whose own value cannot be sent still declares the
    /// stream, and is answered with the 500 at once.
    #[tokio::test]
    async fn an_unsendable_sse_content_type_is_a_500_at_head() {
        let mut leading = unsendable_leading_meta();
        leading[0] = entry(META_RESP_CONTENT_TYPE, "text/event-stream\r\nX: 1");
        leading.pop();
        assert_unsendable_500_at_head(leading).await;
    }

    /// Two content types: the decision follows the last, which is the one
    /// the response carries — both ways round.
    #[tokio::test]
    async fn the_last_content_type_decides_and_is_the_one_sent() {
        let html_then_sse = streamed(
            open_response(events_stream(vec![
                StreamEvent::Meta(entry("resp.header.Content-Type", "text/html")),
                StreamEvent::Meta(sse_meta()),
                chunk(b"data: x\n\n"),
                complete(),
            ]))
            .await,
        );
        assert_eq!(
            header(&html_then_sse.headers, "content-type"),
            Some("text/event-stream")
        );

        let make = || {
            events_stream(vec![
                StreamEvent::Meta(sse_meta()),
                StreamEvent::Meta(entry("resp.header.Content-Type", "text/html")),
                chunk(b"<p>hi</p>"),
                complete(),
            ])
        };
        let sse_then_html = buffered(open_response(make()).await);
        let parts = collect_http_response(sse_then_html).await;
        assert_eq!(header(&parts.headers, "content-type"), Some("text/html"));
        assert_eq!(parts, collect_http_response(make()).await);
    }

    async fn streamed_body_of(rest: Vec<StreamEvent>) -> Vec<Result<Vec<u8>, WaferError>> {
        let mut events = vec![StreamEvent::Meta(sse_meta()), chunk(b"head")];
        events.extend(rest);
        body_items(streamed(open_response(events_stream(events)).await).body).await
    }

    #[tokio::test]
    async fn a_streamed_body_forwards_chunks_and_drops_late_meta() {
        let items = streamed_body_of(vec![
            StreamEvent::Meta(entry("resp.header.X-Late", "1")),
            chunk(b"two"),
            StreamEvent::Complete {
                meta: vec![entry("resp.header.X-Trailing", "1")],
            },
        ])
        .await;
        let items: Vec<Vec<u8>> = items.into_iter().map(Result::unwrap).collect();
        assert_eq!(items, vec![b"head".to_vec(), b"two".to_vec()]);
    }

    /// A `Drop` or `Continue` carries no body, so one after body bytes — a
    /// non-sink producer's protocol violation — ends the body in an `Err`,
    /// as a `Halt` does.
    #[tokio::test]
    async fn a_streamed_body_ends_in_err_on_drop_and_continue() {
        for terminal in [
            StreamEvent::Drop { meta: vec![] },
            StreamEvent::Continue(Message::new("next")),
        ] {
            let items = streamed_body_of(vec![terminal]).await;
            assert_eq!(items.len(), 2, "{items:?}");
            assert!(items[0].is_ok());
            assert!(items[1].is_err());
        }
    }

    async fn length_checked_body(
        declared: &str,
        chunks: &[&[u8]],
    ) -> Vec<Result<Vec<u8>, WaferError>> {
        let mut events = vec![
            StreamEvent::Meta(marker()),
            StreamEvent::Meta(entry(META_RESP_CONTENT_LENGTH, declared)),
        ];
        events.extend(chunks.iter().map(|c| chunk(c)));
        events.push(complete());
        let response = streamed(open_response(events_stream(events)).await);
        body_items(response.body).await
    }

    #[tokio::test]
    async fn a_streamed_body_of_its_declared_length_ends_cleanly() {
        let items = length_checked_body("6", &[b"abc", b"def"]).await;
        let items: Vec<Vec<u8>> = items.into_iter().map(Result::unwrap).collect();
        assert_eq!(items, vec![b"abc".to_vec(), b"def".to_vec()]);
    }

    /// A chunk that would overrun the declared length is an `Err` in its
    /// place: the transport has sent fewer bytes than its `Content-Length`
    /// and aborts, instead of a transport cutting the chunk and ending as
    /// if whole.
    #[tokio::test]
    async fn a_chunk_overrunning_the_declared_length_is_an_err_in_its_place() {
        let items = length_checked_body("4", &[b"abc", b"def"]).await;
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(items[0].as_deref().ok(), Some(&b"abc"[..]));
        assert!(items[1].is_err(), "the overrunning chunk is not sent");
    }

    fn declared_length_events(declared: &str, rest: Vec<StreamEvent>) -> Vec<StreamEvent> {
        let mut events = vec![
            StreamEvent::Meta(marker()),
            StreamEvent::Meta(entry(META_RESP_CONTENT_LENGTH, declared)),
        ];
        events.extend(rest);
        events
    }

    async fn declared_length_body(
        declared: &str,
        rest: Vec<StreamEvent>,
    ) -> Vec<Result<Vec<u8>, WaferError>> {
        let opened = open_response(events_stream(declared_length_events(declared, rest))).await;
        body_items(streamed(opened).body).await
    }

    /// The chunk that meets the declared length exactly is held until the
    /// terminal: when more bytes follow it, it is replaced by an `Err`. A
    /// transport that stops polling once the length is met (hyper) would
    /// otherwise send it and end a truncated body cleanly.
    #[tokio::test]
    async fn an_overrun_starting_on_a_chunk_boundary_is_an_err_in_place_of_the_last_chunk() {
        let items =
            declared_length_body("5", vec![chunk(b"abcde"), chunk(b"fgh"), complete()]).await;
        assert_eq!(items.len(), 1, "{items:?}");
        assert!(
            items[0].is_err(),
            "the chunk meeting the length is never sent"
        );

        let items = declared_length_body(
            "5",
            vec![chunk(b"ab"), chunk(b"cde"), chunk(b"fg"), complete()],
        )
        .await;
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(items[0].as_deref().ok(), Some(&b"ab"[..]));
        assert!(items[1].is_err());
    }

    #[tokio::test]
    async fn the_chunk_meeting_the_declared_length_is_yielded_on_complete() {
        let items = declared_length_body("5", vec![chunk(b"abcde"), complete()]).await;
        let items: Vec<Vec<u8>> = items.into_iter().map(Result::unwrap).collect();
        assert_eq!(items, vec![b"abcde".to_vec()]);

        // Meta and empty chunks between it and the terminal change nothing.
        let items = declared_length_body(
            "5",
            vec![
                chunk(b"abcde"),
                chunk(b""),
                StreamEvent::Meta(entry("resp.header.X-Late", "1")),
                complete(),
            ],
        )
        .await;
        let items: Vec<Vec<u8>> = items.into_iter().map(Result::unwrap).collect();
        assert_eq!(items, vec![b"abcde".to_vec()]);
    }

    /// While the producer has not sent its terminal, the chunk that met the
    /// length is not yielded.
    #[tokio::test]
    async fn the_chunk_meeting_the_declared_length_waits_for_the_terminal() {
        use futures::{FutureExt, StreamExt};
        let (stream, sink, _cancel) = OutputStream::new_streaming();
        sink.send_meta(marker()).await.unwrap();
        sink.send_meta(entry(META_RESP_CONTENT_LENGTH, "5"))
            .await
            .unwrap();
        sink.send_chunk(b"abcde".to_vec()).await.unwrap();
        let mut body = streamed(open_live(stream).await).body;
        assert!(
            body.next().now_or_never().is_none(),
            "held until the producer's terminal"
        );
        sink.complete(vec![]).await.unwrap();
        assert_eq!(body.next().await.unwrap().unwrap(), b"abcde");
        assert!(body.next().await.is_none());
    }

    /// Contradictions known before the head — any body byte under a declared
    /// `0`, a first chunk already longer than the declared length — are
    /// answered with the 500 at once, and the producer is cancelled.
    #[tokio::test]
    async fn a_body_contradicting_its_declared_length_at_head_is_a_500() {
        for (declared, events) in [
            ("0", vec![chunk(b"x")]),
            (
                "0",
                vec![chunk(b""), StreamEvent::Meta(entry("m", "1")), chunk(b"x")],
            ),
            ("3", vec![chunk(b"abcde")]),
        ] {
            let (stream, sink, cancel) = OutputStream::new_streaming();
            let leading = vec![marker(), entry(META_RESP_CONTENT_LENGTH, declared)];
            for entry in &leading {
                sink.send_meta(entry.clone()).await.unwrap();
            }
            for event in events {
                match event {
                    StreamEvent::Chunk(bytes) => sink.send_chunk(bytes).await.unwrap(),
                    StreamEvent::Meta(entry) => sink.send_meta(entry).await.unwrap(),
                    _ => unreachable!(),
                }
            }
            let opened = buffered(open_live(stream).await);
            assert!(cancel.is_cancelled(), "declared {declared}");
            assert_eq!(
                collect_http_response(opened).await,
                unsendable_response(&leading, &body_exceeds_content_length()),
                "declared {declared}"
            );
            drop(sink);
        }
    }

    /// A declared `0` with no body bytes streams an empty body that ends
    /// cleanly; one that ends in an error before any byte renders buffered,
    /// as any declared stream that ends before its body does.
    #[tokio::test]
    async fn a_declared_zero_length_is_checked_to_the_terminal() {
        let items = declared_length_body(
            "0",
            vec![
                chunk(b""),
                StreamEvent::Meta(entry("resp.header.X-Late", "1")),
                complete(),
            ],
        )
        .await;
        assert!(items.is_empty(), "{items:?}");

        let opened = open_response(events_stream(declared_length_events(
            "0",
            vec![
                chunk(b""),
                StreamEvent::Error(Box::new(WaferError::new(ErrorCode::NotFound, "gone"))),
            ],
        )))
        .await;
        let parts = collect_http_response(buffered(opened)).await;
        assert_eq!(parts.status, 404);
    }

    #[tokio::test]
    async fn a_body_ending_short_of_its_declared_length_ends_in_err() {
        let items = length_checked_body("7", &[b"abc", b"def"]).await;
        assert_eq!(items.len(), 3, "{items:?}");
        assert!(items[..2].iter().all(Result::is_ok));
        assert!(items[2].is_err(), "a short body must not end cleanly");
    }

    /// Every ending that leaves the bytes sent a truncated prefix is a final
    /// `Err`, so a transport aborts the body instead of ending it cleanly.
    #[tokio::test]
    async fn a_streamed_body_ends_in_err_on_error_halt_and_a_missing_terminal() {
        let error = streamed_body_of(vec![
            chunk(b"partial"),
            StreamEvent::Error(Box::new(WaferError::new(
                ErrorCode::Unavailable,
                "read failed",
            ))),
        ])
        .await;
        assert_eq!(error.len(), 3, "{error:?}");
        assert_eq!(
            error[2].as_ref().unwrap_err().message,
            "read failed",
            "the producer's error is the final item"
        );

        let halt = streamed_body_of(vec![StreamEvent::Halt {
            body: b"a whole other response".to_vec(),
            meta: vec![],
        }])
        .await;
        assert_eq!(halt.len(), 2, "{halt:?}");
        assert!(
            halt[1].is_err(),
            "a Halt after the body started must not end cleanly"
        );

        let missing = streamed_body_of(vec![chunk(b"partial")]).await;
        assert_eq!(missing.len(), 3, "{missing:?}");
        assert!(
            missing[2].is_err(),
            "a body with no terminal must not end cleanly"
        );
    }

    /// A producer that stops mid-body without a terminal hands the body
    /// wafer-block's dropped-sink `Error`, which ends it in an `Err`.
    #[tokio::test]
    async fn a_streamed_body_ends_in_err_when_its_producer_stops_without_a_terminal() {
        let output = OutputStream::from_producer(|sink, _cancel| async move {
            sink.send_meta(sse_meta()).await.ok();
            sink.send_chunk(b"partial".to_vec()).await.ok();
        });
        let items = body_items(streamed(open_response(output).await).body).await;
        assert!(
            items.last().is_some_and(Result::is_err),
            "a producer cut off mid-body must not end cleanly: {items:?}"
        );
    }

    /// Nothing is read after a body's final item.
    #[tokio::test]
    async fn a_streamed_body_yields_nothing_after_its_end() {
        use futures::StreamExt;
        let mut body = streamed(
            open_response(events_stream(vec![
                StreamEvent::Meta(sse_meta()),
                chunk(b"head"),
                StreamEvent::Error(Box::new(WaferError::new(ErrorCode::Internal, "x"))),
                chunk(b"after the terminal"),
            ]))
            .await,
        )
        .body;
        assert!(body.next().await.unwrap().is_ok());
        assert!(body.next().await.unwrap().is_err());
        assert!(body.next().await.is_none());
        assert!(body.next().await.is_none());
    }

    /// `terminal_to_http_response` over a collection is what
    /// `collect_http_response` renders for the stream, for every terminal.
    #[tokio::test]
    async fn terminal_to_http_response_renders_as_collect_http_response() {
        let cases: Vec<Box<dyn Fn() -> OutputStream>> = vec![
            Box::new(|| {
                OutputStream::respond_with_meta(
                    b"body".to_vec(),
                    vec![entry(META_RESP_STATUS, "201")],
                )
            }),
            Box::new(|| {
                OutputStream::halt(b"halted".to_vec(), vec![entry(META_RESP_STATUS, "403")])
            }),
            Box::new(|| OutputStream::error(WaferError::new(ErrorCode::PermissionDenied, "nope"))),
            Box::new(|| {
                OutputStream::drop_request_with_meta(vec![entry(
                    "resp.header.Access-Control-Allow-Origin",
                    "https://a.example",
                )])
            }),
            Box::new(|| OutputStream::continue_with(Message::new("next"))),
            Box::new(|| events_stream(vec![chunk(b"no terminal")])),
            Box::new(|| {
                OutputStream::respond_with_meta(
                    b"x".to_vec(),
                    vec![entry("resp.header.X-Bad", "\n")],
                )
            }),
        ];
        for make in cases {
            assert_eq!(
                terminal_to_http_response(make().collect_buffered().await),
                collect_http_response(make()).await
            );
        }
        let malformed =
            terminal_to_http_response(events_stream(vec![chunk(b"x")]).collect_buffered().await);
        assert_eq!(malformed.status, 500);
        assert_eq!(malformed.body, b"internal server error");
    }
}
