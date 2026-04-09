pub mod config;
pub mod crypto;
pub mod database;
pub mod logger;
pub mod network;
pub mod storage;

use wafer_block::common::ErrorCode;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::context::Context;
use wafer_block::meta::{META_WRAP_ACCESS, META_WRAP_RESOURCE, META_WRAP_RESOURCE_TYPE};
use wafer_block::{Action, Message, WaferError};

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

pub(crate) use {dual_api, svc, svc_fn};

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
    let payload = serde_json::to_vec(data)
        .map_err(|e| WaferError::new(ErrorCode::INTERNAL, e.to_string()))?;
    let mut msg = Message::new(kind, payload);
    if let Some(res) = resource {
        msg.set_meta(META_WRAP_RESOURCE, res);
        msg.set_meta(META_WRAP_ACCESS, if is_write { "write" } else { "read" });
        if let Some(rt) = resource_type {
            msg.set_meta(META_WRAP_RESOURCE_TYPE, rt);
        }
    }
    let result = ctx.call_block(block, &mut msg).await;
    match result.action {
        Action::Error => Err(result
            .error
            .unwrap_or_else(|| WaferError::new(ErrorCode::INTERNAL, "unknown error"))),
        _ => Ok(result.response.map(|r| r.data).unwrap_or_default()),
    }
}

/// Call a block and return the raw response bytes (WASM sync variant).
/// Uses the WIT `runtime::call-block` host import instead of `ctx.call_block()`.
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
    let payload = serde_json::to_vec(data)
        .map_err(|e| WaferError::new(ErrorCode::INTERNAL, e.to_string()))?;
    let mut msg = Message::new(kind, payload);
    if let Some(res) = resource {
        msg.set_meta(META_WRAP_RESOURCE, res);
        msg.set_meta(META_WRAP_ACCESS, if is_write { "write" } else { "read" });
        if let Some(rt) = resource_type {
            msg.set_meta(META_WRAP_RESOURCE_TYPE, rt);
        }
    }
    let result = wafer_block::runtime::call_block(block, &msg);
    match result.action {
        Action::Error => Err(result
            .error
            .unwrap_or_else(|| WaferError::new(ErrorCode::INTERNAL, "unknown error"))),
        _ => Ok(result.response.map(|r| r.data).unwrap_or_default()),
    }
}

/// Deserialize JSON bytes into a typed value.
pub(crate) fn decode<T: serde::de::DeserializeOwned>(data: &[u8]) -> Result<T, WaferError> {
    serde_json::from_slice(data)
        .map_err(|e| WaferError::new(ErrorCode::INTERNAL, format!("decode error: {e}")))
}
