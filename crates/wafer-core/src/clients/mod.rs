/// Typed RPC client for the auth service (sessions, user lookup, role checks).
pub mod auth;
/// Typed RPC client for reading typed config values via the runtime's config block.
pub mod config;
/// Typed RPC client for the crypto service (hash/verify, JWT issue/verify, random bytes).
pub mod crypto;
/// Typed RPC client for the database service (CRUD, query, migration helpers).
pub mod database;
/// Typed RPC client for the image service (transform, encode, metadata).
pub mod image;
/// Typed RPC client for the LLM service (text generation, embeddings, tools).
pub mod llm;
/// Typed RPC client for the logger service.
pub mod logger;
/// Typed RPC client for the network service (outbound HTTP requests).
pub mod network;
/// Typed RPC client for the storage service (object put/get/list/delete).
pub mod storage;
/// Typed RPC client for the vector + embedding service (upsert, search, hybrid).
pub mod vector;

#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::meta::{
    META_REQ_ACTION, META_WRAP_ACCESS, META_WRAP_RESOURCE, META_WRAP_RESOURCE_TYPE,
};
#[cfg(not(feature = "wasm-component"))]
use wafer_block::streams::input::InputStream;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::streams::output::OutputStream;
use wafer_block::{codec, common::ErrorCode, Message, WaferError};

// ---------------------------------------------------------------------------
// Macros for generating cfg-gated native-async / wasm-sync function pairs.
//
// These eliminate the ~600 lines of duplicated client code. Each client
// function is defined once; the macros produce two cfg-gated variants:
//   native:  `pub async fn name(ctx: &dyn Context, ...) -> R { ... }`
//   wasm:    `pub fn name(...) -> R { ... }`
// ---------------------------------------------------------------------------

/// Generate a cfg-gated function pair from a single definition.
///
/// The first identifier in the parameter list names the Context parameter
/// (conventionally `ctx`). In native mode it becomes `ctx: &dyn Context`;
/// in wasm-component mode it is omitted from the signature. Inside the body,
/// use `svc!(ctx, ...)` and `svc_fn!(ctx, ...)` to call services / other
/// dual_api functions — they handle `.await` automatically.
macro_rules! dual_api {
    (
        $(
            $(#[$meta:meta])*
            $vis:vis fn $name:ident( $ctx:ident, $($param:ident : $ty:ty),* $(,)? ) -> $ret:ty $body:block
        )*
    ) => {
        $(
            #[cfg(not(feature = "wasm-component"))]
            $(#[$meta])*
            $vis async fn $name($ctx: &dyn Context, $($param : $ty),*) -> $ret $body

            #[cfg(feature = "wasm-component")]
            $(#[$meta])*
            $vis fn $name($($param : $ty),*) -> $ret $body
        )*
    };
}

/// Call `call_service` with the right ctx/await for the active cfg.
/// The first argument is the context identifier from `dual_api!`.
macro_rules! svc {
    ($ctx:ident, $block:expr, $kind:expr, $data:expr, $resource:expr, $write:expr, $rt:expr) => {{
        #[cfg(not(feature = "wasm-component"))]
        let __r = call_service($ctx, $block, $kind, $data, $resource, $write, $rt).await;
        #[cfg(feature = "wasm-component")]
        let __r = call_service($block, $kind, $data, $resource, $write, $rt);
        __r
    }};
}

/// Like `svc!`, but forwards a pre-built `Message` (cloned from the caller's
/// incoming msg + mutated as needed) instead of constructing a fresh message
/// from `(kind, data)`. Used by typed clients that must propagate request
/// meta (auth headers, cookies, etc.) to the downstream service.
macro_rules! svc_msg {
    ($ctx:ident, $block:expr, $msg:expr, $resource:expr, $write:expr, $rt:expr) => {{
        #[cfg(not(feature = "wasm-component"))]
        let __r = call_service_with_msg($ctx, $block, $msg, $resource, $write, $rt).await;
        #[cfg(feature = "wasm-component")]
        let __r = call_service_with_msg($block, $msg, $resource, $write, $rt);
        __r
    }};
}

/// Call another `dual_api!` function with the right ctx/await for the active cfg.
/// The first argument is the context identifier from `dual_api!`.
macro_rules! svc_fn {
    ($ctx:ident, $fn:ident ( $($args:expr),* $(,)? )) => {{
        #[cfg(not(feature = "wasm-component"))]
        let __r = $fn($ctx, $($args),*).await;
        #[cfg(feature = "wasm-component")]
        let __r = $fn($($args),*);
        __r
    }};
}

pub(crate) use dual_api;
pub(crate) use svc;
pub(crate) use svc_fn;
pub(crate) use svc_msg;

/// Call a block and return the raw response bytes (native async variant).
/// Returns `Err(WaferError)` if the block returns an error.
///
/// If `resource` is `Some`, sets WRAP meta so the runtime can enforce access control.
/// `resource_type` scopes the grant check to a specific service (e.g. `Some("db")`).
#[cfg(not(feature = "wasm-component"))]
pub(crate) async fn call_service(
    ctx: &dyn Context,
    block: &str,
    kind: &str,
    data: &impl serde::Serialize,
    resource: Option<&str>,
    is_write: bool,
    resource_type: Option<&str>,
) -> Result<Vec<u8>, WaferError> {
    let out =
        call_service_streaming(ctx, block, kind, data, resource, is_write, resource_type).await?;
    match out.collect_buffered().await {
        Ok(buf) => Ok(buf.body),
        Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => Err(e),
        Err(wafer_block::streams::output::TerminalNotResponse::Drop) => {
            Err(WaferError::new(ErrorCode::INTERNAL, "block returned Drop"))
        }
        Err(wafer_block::streams::output::TerminalNotResponse::Continue(_)) => Err(
            WaferError::new(ErrorCode::INTERNAL, "block returned Continue"),
        ),
        Err(wafer_block::streams::output::TerminalNotResponse::Malformed) => Err(WaferError::new(
            ErrorCode::INTERNAL,
            "malformed output stream",
        )),
    }
}

/// Call a block and return the raw response bytes (WASM sync variant).
/// Uses the WASM ABI host import to call another block synchronously.
///
/// If `resource` is `Some`, sets WRAP meta so the runtime can enforce access control.
/// `resource_type` scopes the grant check to a specific service (e.g. `Some("db")`).
#[cfg(feature = "wasm-component")]
pub(crate) fn call_service(
    block: &str,
    kind: &str,
    data: &impl serde::Serialize,
    resource: Option<&str>,
    is_write: bool,
    resource_type: Option<&str>,
) -> Result<Vec<u8>, WaferError> {
    let _ = (block, kind, data, resource, is_write, resource_type);
    // TODO(#103): implement WASM sync call_block via ABI host import when
    // redesigning the WASM component path for the streaming protocol.
    Err(WaferError::new(
        ErrorCode::UNIMPLEMENTED,
        "wasm-component call_service not yet implemented for streaming protocol",
    ))
}

/// Native: call a block and return the raw `OutputStream` without buffering.
///
/// Use this when callers need frame-by-frame access to the response — for
/// example, the network client needs to peel off the `ResponseHeader` frame
/// before the body chunks, and `collect_buffered` (used by [`call_service`])
/// would concatenate every `Chunk` event into one blob, destroying the frame
/// boundary. For single-frame services, [`call_service`] is simpler.
#[cfg(not(feature = "wasm-component"))]
pub(crate) async fn call_service_streaming(
    ctx: &dyn Context,
    block: &str,
    kind: &str,
    data: &impl serde::Serialize,
    resource: Option<&str>,
    is_write: bool,
    resource_type: Option<&str>,
) -> Result<OutputStream, WaferError> {
    let payload = codec::encode(data)?;
    let mut msg = Message::new(kind);
    msg.set_meta(META_REQ_ACTION, kind);
    if let Some(res) = resource {
        msg.set_meta(META_WRAP_RESOURCE, res);
        msg.set_meta(META_WRAP_ACCESS, if is_write { "write" } else { "read" });
        if let Some(rt) = resource_type {
            msg.set_meta(META_WRAP_RESOURCE_TYPE, rt);
        }
    }
    Ok(ctx
        .call_block(block, msg, InputStream::from_bytes(payload))
        .await)
}

/// Call a block, forwarding a pre-built `Message` (carrying request meta from
/// the caller's incoming message — e.g. Authorization / Cookie headers, etc.)
/// and return the raw response bytes.
///
/// Used by typed clients that must propagate caller-side request meta to a
/// downstream service (e.g. `clients::auth::require_user`, which forwards the
/// caller's `Message` so the auth service can read Bearer / Cookie headers
/// from the original request). The caller is responsible for setting `kind`
/// on the message; this function will (re)apply WRAP meta if `resource` is
/// `Some` and ensure `META_REQ_ACTION` matches `msg.kind`.
#[cfg(not(feature = "wasm-component"))]
pub(crate) async fn call_service_with_msg(
    ctx: &dyn Context,
    block: &str,
    mut msg: Message,
    resource: Option<&str>,
    is_write: bool,
    resource_type: Option<&str>,
) -> Result<Vec<u8>, WaferError> {
    msg.set_meta(META_REQ_ACTION, msg.kind.clone());
    if let Some(res) = resource {
        msg.set_meta(META_WRAP_RESOURCE, res);
        msg.set_meta(META_WRAP_ACCESS, if is_write { "write" } else { "read" });
        if let Some(rt) = resource_type {
            msg.set_meta(META_WRAP_RESOURCE_TYPE, rt);
        }
    }
    let out = ctx
        .call_block(block, msg, InputStream::from_bytes(Vec::new()))
        .await;
    match out.collect_buffered().await {
        Ok(buf) => Ok(buf.body),
        Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => Err(e),
        Err(wafer_block::streams::output::TerminalNotResponse::Drop) => {
            Err(WaferError::new(ErrorCode::INTERNAL, "block returned Drop"))
        }
        Err(wafer_block::streams::output::TerminalNotResponse::Continue(_)) => Err(
            WaferError::new(ErrorCode::INTERNAL, "block returned Continue"),
        ),
        Err(wafer_block::streams::output::TerminalNotResponse::Malformed) => Err(WaferError::new(
            ErrorCode::INTERNAL,
            "malformed output stream",
        )),
    }
}

/// WASM-component variant of [`call_service_with_msg`]. Currently
/// unimplemented — same status as the other WASM-component entry points.
#[cfg(feature = "wasm-component")]
pub(crate) fn call_service_with_msg(
    block: &str,
    msg: Message,
    resource: Option<&str>,
    is_write: bool,
    resource_type: Option<&str>,
) -> Result<Vec<u8>, WaferError> {
    let _ = (block, msg, resource, is_write, resource_type);
    Err(WaferError::new(
        ErrorCode::UNIMPLEMENTED,
        "wasm-component call_service_with_msg not yet implemented for streaming protocol",
    ))
}

/// Deserialize MessagePack bytes into a typed value.
pub(crate) fn decode<T: serde::de::DeserializeOwned>(data: &[u8]) -> Result<T, WaferError> {
    codec::decode(data)
        .map_err(|e| WaferError::new(ErrorCode::INTERNAL, format!("decode error: {}", e.message)))
}

/// Pull events from `out` until the first `Chunk` (the header frame), decode
/// it as `H`, and return it. Skips `Meta` events. Any non-`Chunk` terminal
/// arriving before the header is mapped to a `WaferError` whose message is
/// prefixed by `context` so callers can attribute the failure to a specific
/// service operation (e.g. `"network do_request"`).
///
/// Used by services whose handlers emit a typed header frame followed by
/// zero-or-more body chunks (network, storage GET, …).
#[cfg(not(feature = "wasm-component"))]
pub(crate) async fn read_header_frame<H>(
    out: &mut OutputStream,
    context: &str,
) -> Result<H, WaferError>
where
    H: serde::de::DeserializeOwned,
{
    use futures::StreamExt;
    use wafer_block::stream::StreamEvent;
    while let Some(evt) = out.next().await {
        match evt {
            StreamEvent::Chunk(bytes) => {
                return codec::decode::<H>(&bytes).map_err(|e| {
                    WaferError::new(e.code, format!("{context} header decode: {}", e.message))
                });
            }
            StreamEvent::Meta(_) => continue,
            StreamEvent::Error(e) => return Err(*e),
            StreamEvent::Drop => {
                return Err(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("{context}: block dropped before header frame"),
                ));
            }
            StreamEvent::Continue(msg) => {
                return Err(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!(
                        "{context}: unexpected Continue before header frame (kind: {})",
                        msg.kind
                    ),
                ));
            }
            StreamEvent::Complete { .. } => {
                return Err(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("{context}: stream complete before header frame"),
                ));
            }
        }
    }
    Err(WaferError::new(
        ErrorCode::INTERNAL,
        format!("{context}: stream ended before header frame"),
    ))
}

/// Read header (decoded as `H`) plus the accumulated body chunks from a
/// two-frame response.
///
/// Frame 1 is decoded as `H` via [`read_header_frame`]; subsequent `Chunk`
/// events are concatenated into the body. Non-`Complete` terminals are mapped
/// to `WaferError`. A stream that ends without any terminal event is reported
/// as a malformed protocol violation, prefixed with `context`.
#[cfg(not(feature = "wasm-component"))]
pub(crate) async fn buffered_header_and_body<H>(
    mut out: OutputStream,
    context: &str,
) -> Result<(H, Vec<u8>), WaferError>
where
    H: serde::de::DeserializeOwned,
{
    use futures::StreamExt;
    use wafer_block::stream::StreamEvent;
    let header: H = read_header_frame(&mut out, context).await?;
    let mut body = Vec::new();
    while let Some(evt) = out.next().await {
        match evt {
            StreamEvent::Chunk(bytes) => body.extend_from_slice(&bytes),
            StreamEvent::Meta(_) => continue,
            StreamEvent::Complete { .. } => return Ok((header, body)),
            StreamEvent::Error(e) => return Err(*e),
            StreamEvent::Drop => {
                return Err(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("{context}: block dropped"),
                ));
            }
            StreamEvent::Continue(msg) => {
                return Err(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("{context}: unexpected Continue (kind: {})", msg.kind),
                ));
            }
        }
    }
    Err(WaferError::new(
        ErrorCode::INTERNAL,
        format!("{context}: stream ended without terminal event"),
    ))
}
