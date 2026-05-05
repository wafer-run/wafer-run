pub mod config;
pub mod crypto;
pub mod database;
pub mod logger;
pub mod network;
pub mod storage;
pub mod vector;

#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::streams::input::InputStream;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::streams::output::OutputStream;
use wafer_block::{
    codec,
    common::ErrorCode,
    meta::{META_REQ_ACTION, META_WRAP_ACCESS, META_WRAP_RESOURCE, META_WRAP_RESOURCE_TYPE},
    Message, WaferError,
};

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
    let payload = codec::encode(data)?;
    let mut msg = Message::new(kind);
    // Set META_REQ_ACTION so call_block's interface-action validator can check
    // the action against the target block's declared interface spec.
    msg.set_meta(META_REQ_ACTION, kind);
    if let Some(res) = resource {
        msg.set_meta(META_WRAP_RESOURCE, res);
        msg.set_meta(META_WRAP_ACCESS, if is_write { "write" } else { "read" });
        if let Some(rt) = resource_type {
            msg.set_meta(META_WRAP_RESOURCE_TYPE, rt);
        }
    }
    let out = ctx
        .call_block(block, msg, InputStream::from_bytes(payload))
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
    // TODO: implement WASM sync call_block via ABI host import when redesigning
    // the WASM component path for the streaming protocol.
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

/// WASM-component variant of [`call_service_streaming`]. Currently
/// unimplemented — the WASM-component path will be redesigned alongside the
/// streaming protocol.
#[cfg(feature = "wasm-component")]
pub(crate) fn call_service_streaming(
    block: &str,
    kind: &str,
    data: &impl serde::Serialize,
    resource: Option<&str>,
    is_write: bool,
    resource_type: Option<&str>,
) -> Result<(), WaferError> {
    let _ = (block, kind, data, resource, is_write, resource_type);
    Err(WaferError::new(
        ErrorCode::UNIMPLEMENTED,
        "wasm-component call_service_streaming not yet implemented for streaming protocol",
    ))
}

/// Deserialize MessagePack bytes into a typed value.
pub(crate) fn decode<T: serde::de::DeserializeOwned>(data: &[u8]) -> Result<T, WaferError> {
    codec::decode(data)
        .map_err(|e| WaferError::new(ErrorCode::INTERNAL, format!("decode error: {}", e.message)))
}
