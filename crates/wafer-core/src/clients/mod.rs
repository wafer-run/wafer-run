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
// Used by the shared `build_service_message`/`apply_wrap_meta` helpers, which
// run on both the native-async and wasm-component paths.
use wafer_block::meta::{
    META_REQ_ACTION, META_WRAP_ACCESS, META_WRAP_RESOURCE, META_WRAP_RESOURCE_TYPE,
};
#[cfg(not(feature = "wasm-component"))]
use wafer_block::streams::input::InputStream;
#[cfg(not(feature = "wasm-component"))]
use wafer_block::streams::output::OutputStream;
use wafer_block::{codec, common::ErrorCode, Message, WaferError};

/// Apply the WRAP access-control meta to `msg` when a `resource` is supplied,
/// so the runtime can enforce access control. `resource_type` scopes the grant
/// check to a specific service (e.g. `Some("db")`). Shared by the native and
/// wasm-component service-call paths so the emitted meta is byte-identical.
fn apply_wrap_meta(
    msg: &mut Message,
    resource: Option<&str>,
    is_write: bool,
    resource_type: Option<&str>,
) {
    if let Some(res) = resource {
        msg.set_meta(META_WRAP_RESOURCE, res);
        msg.set_meta(META_WRAP_ACCESS, if is_write { "write" } else { "read" });
        if let Some(rt) = resource_type {
            msg.set_meta(META_WRAP_RESOURCE_TYPE, rt);
        }
    }
}

/// Build the request `Message` for a `kind`-based service call: sets
/// `META_REQ_ACTION` to `kind` and (when `resource` is set) the WRAP meta.
/// Shared by the native [`call_service_streaming`] and the wasm-component
/// [`call_service`] so both construct the identical request message.
fn build_service_message(
    kind: &str,
    resource: Option<&str>,
    is_write: bool,
    resource_type: Option<&str>,
) -> Message {
    let mut msg = Message::new(kind);
    msg.set_meta(META_REQ_ACTION, kind);
    apply_wrap_meta(&mut msg, resource, is_write, resource_type);
    msg
}

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
        Err(wafer_block::streams::output::TerminalNotResponse::Halt(_)) => Err(WaferError::new(
            ErrorCode::INTERNAL,
            "block returned Halt (service clients do not short-circuit flows)",
        )),
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
    let body = codec::encode(data)?;
    let msg = build_service_message(kind, resource, is_write, resource_type);
    let msg_bytes = codec::encode(&msg)?;
    wasm_streaming::run(block, &msg_bytes, &body)
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
    let msg = build_service_message(kind, resource, is_write, resource_type);
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
    apply_wrap_meta(&mut msg, resource, is_write, resource_type);
    let out = ctx
        .call_block(block, msg, InputStream::from_bytes(Vec::new()))
        .await;
    match out.collect_buffered().await {
        Ok(buf) => Ok(buf.body),
        Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => Err(e),
        Err(wafer_block::streams::output::TerminalNotResponse::Drop) => {
            Err(WaferError::new(ErrorCode::INTERNAL, "block returned Drop"))
        }
        Err(wafer_block::streams::output::TerminalNotResponse::Halt(_)) => Err(WaferError::new(
            ErrorCode::INTERNAL,
            "block returned Halt (service clients do not short-circuit flows)",
        )),
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
    mut msg: Message,
    resource: Option<&str>,
    is_write: bool,
    resource_type: Option<&str>,
) -> Result<Vec<u8>, WaferError> {
    msg.set_meta(META_REQ_ACTION, msg.kind.clone());
    apply_wrap_meta(&mut msg, resource, is_write, resource_type);
    let msg_bytes = codec::encode(&msg)?;
    // No request body — mirrors the native variant's empty `InputStream`.
    wasm_streaming::run(block, &msg_bytes, &[])
}

/// Synchronous guest-side driver over the streaming ABI host imports, used by
/// the wasm-component [`call_service`] / [`call_service_with_msg`] entry points.
///
/// This mirrors the guest-side driver in `wafer-sdk`'s `stream.rs`,
/// reimplemented here so `wafer-core` carries **no** dependency on the guest
/// SDK (the runtime-side crate must not depend on the guest-facing one). It
/// drives the same `__wafer_host_stream_*` imports the host registers in
/// `build_linker` under the `"wafer"` module:
/// `init → write_chunk → finish → read* → close`.
///
/// The sync-guest ⇄ async-host boundary is handled entirely host-side: the
/// guest's `_finish`/`_read_chunk` calls trap, the host resume loop runs the
/// `async` dispatch / stream drive, then resumes the guest with the result.
/// No `block_on`, no new host machinery — see TODO #103 design doc.
#[cfg(feature = "wasm-component")]
mod wasm_streaming {
    use wafer_block::{codec, common::ErrorCode, WaferError};

    #[cfg(target_arch = "wasm32")]
    #[link(wasm_import_module = "wafer")]
    extern "C" {
        fn __wafer_host_stream_init(
            name_ptr: i32,
            name_len: i32,
            msg_ptr: i32,
            msg_len: i32,
        ) -> i64;
        fn __wafer_host_stream_write_chunk(handle: i64, body_ptr: i32, body_len: i32) -> i32;
        fn __wafer_host_stream_finish(handle: i64) -> i32;
        fn __wafer_host_stream_read_chunk(handle: i64) -> i64;
        fn __wafer_host_stream_take_error(handle: i64) -> i64;
        fn __wafer_host_stream_close(handle: i64);
    }

    /// Drive one full request/response cycle and return the concatenated
    /// response body. `msg_bytes` is the rmp-encoded request `Message`; `body`
    /// is the rmp-encoded request payload (empty for the `call_service_with_msg`
    /// path, which carries no body).
    pub(super) fn run(block: &str, msg_bytes: &[u8], body: &[u8]) -> Result<Vec<u8>, WaferError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            // The host imports only exist inside a wasm32 guest. The
            // `wasm-component` feature is, in practice, only enabled for wasm32
            // component builds; this arm keeps the crate compilable (and
            // honestly diagnosable) if the feature is toggled on a host target.
            let _ = (block, msg_bytes, body);
            Err(WaferError::new(
                ErrorCode::Unimplemented,
                "wasm-component call_service is only available in wasm32 guests",
            ))
        }
        #[cfg(target_arch = "wasm32")]
        // SAFETY: every pointer/length handed to or received from the host is
        // sized by the streaming-ABI contract. Response buffers are
        // host-allocated via the guest `__wafer_alloc` and reclaimed here by
        // reconstructing the owning `Vec`. `_guard` closes the handle on every
        // exit path (including early returns and panics).
        unsafe {
            let handle = __wafer_host_stream_init(
                block.as_ptr() as i32,
                block.len() as i32,
                msg_bytes.as_ptr() as i32,
                msg_bytes.len() as i32,
            );
            if handle < 0 {
                // No handle was allocated → nothing to close.
                return Err(WaferError::new(
                    ErrorCode::from_ordinal(sentinel_ordinal(handle)),
                    format!("stream_init failed for {block}"),
                ));
            }
            let _guard = CloseGuard(handle);

            if !body.is_empty() {
                let r = __wafer_host_stream_write_chunk(
                    handle,
                    body.as_ptr() as i32,
                    body.len() as i32,
                );
                if r < 0 {
                    return Err(WaferError::new(
                        ErrorCode::from_ordinal(sentinel_ordinal(r as i64)),
                        "stream_write_chunk failed",
                    ));
                }
            }

            let r = __wafer_host_stream_finish(handle);
            if r < 0 {
                return Err(WaferError::new(
                    ErrorCode::from_ordinal(sentinel_ordinal(r as i64)),
                    "stream_finish failed",
                ));
            }

            let mut out = Vec::new();
            loop {
                let packed = __wafer_host_stream_read_chunk(handle);
                if packed == 0 {
                    break; // end of stream
                }
                if packed < 0 {
                    return Err(take_error(handle));
                }
                let (ptr, len) = unpack_ptr_len(packed);
                out.extend_from_slice(core::slice::from_raw_parts(ptr as *const u8, len));
                // Reclaim the host-allocated guest buffer.
                drop(Vec::from_raw_parts(ptr as *mut u8, len, len));
            }
            Ok(out)
        }
    }

    /// Map a negative streaming-ABI sentinel to its `ErrorCode` ordinal (the
    /// low byte of the sentinel's absolute value).
    #[cfg(target_arch = "wasm32")]
    fn sentinel_ordinal(sentinel: i64) -> u8 {
        (sentinel.unsigned_abs() & 0xFF) as u8
    }

    /// Unpack a positive packed `(ptr, len)` i64 into `(ptr, len)` as `usize`.
    #[cfg(target_arch = "wasm32")]
    fn unpack_ptr_len(packed: i64) -> (usize, usize) {
        let ptr = (packed >> 32) as u32 as usize;
        let len = (packed & 0xFFFF_FFFF) as usize;
        (ptr, len)
    }

    /// Fetch the full structured `WaferError` for a handle after a negative
    /// read sentinel, falling back to a generic `Internal` error if absent or
    /// undecodable.
    ///
    /// # Safety
    /// `handle` must be a live stream handle owned by the caller.
    #[cfg(target_arch = "wasm32")]
    unsafe fn take_error(handle: i64) -> WaferError {
        let packed = __wafer_host_stream_take_error(handle);
        if packed > 0 {
            let (ptr, len) = unpack_ptr_len(packed);
            let bytes = core::slice::from_raw_parts(ptr as *const u8, len).to_vec();
            // Reclaim the host-allocated guest buffer.
            drop(Vec::from_raw_parts(ptr as *mut u8, len, len));
            return codec::decode(&bytes).unwrap_or_else(|_| {
                WaferError::new(ErrorCode::Internal, "stream error (undecodable)")
            });
        }
        WaferError::new(ErrorCode::Internal, "stream error (no detail)")
    }

    /// RAII guard that closes the host stream handle on every exit path.
    /// `__wafer_host_stream_close` is idempotent on the host.
    #[cfg(target_arch = "wasm32")]
    struct CloseGuard(i64);

    #[cfg(target_arch = "wasm32")]
    impl Drop for CloseGuard {
        fn drop(&mut self) {
            // SAFETY: handle is valid for the guard's lifetime; close is idempotent.
            unsafe { __wafer_host_stream_close(self.0) }
        }
    }
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
            StreamEvent::Halt { .. } => {
                return Err(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("{context}: block halted before header frame"),
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
            StreamEvent::Halt { .. } => {
                return Err(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("{context}: block halted"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn meta<'a>(msg: &'a Message, key: &str) -> Option<&'a str> {
        msg.meta
            .iter()
            .find(|e| e.key == key)
            .map(|e| e.value.as_str())
    }

    #[test]
    fn build_service_message_sets_kind_and_action() {
        let msg = build_service_message("get", None, false, None);
        assert_eq!(msg.kind, "get");
        assert_eq!(meta(&msg, META_REQ_ACTION), Some("get"));
        // No resource → no WRAP meta at all.
        assert_eq!(meta(&msg, META_WRAP_RESOURCE), None);
        assert_eq!(meta(&msg, META_WRAP_ACCESS), None);
        assert_eq!(meta(&msg, META_WRAP_RESOURCE_TYPE), None);
    }

    #[test]
    fn build_service_message_write_resource_sets_full_wrap_meta() {
        let msg = build_service_message("create", Some("users"), true, Some("db"));
        assert_eq!(meta(&msg, META_REQ_ACTION), Some("create"));
        assert_eq!(meta(&msg, META_WRAP_RESOURCE), Some("users"));
        assert_eq!(meta(&msg, META_WRAP_ACCESS), Some("write"));
        assert_eq!(meta(&msg, META_WRAP_RESOURCE_TYPE), Some("db"));
    }

    #[test]
    fn build_service_message_read_access_and_omitted_resource_type() {
        let msg = build_service_message("list", Some("orders"), false, None);
        assert_eq!(meta(&msg, META_WRAP_ACCESS), Some("read"));
        assert_eq!(meta(&msg, META_WRAP_RESOURCE), Some("orders"));
        // resource_type omitted → key absent (not empty string).
        assert_eq!(meta(&msg, META_WRAP_RESOURCE_TYPE), None);
    }

    #[test]
    fn apply_wrap_meta_none_resource_is_noop() {
        let mut msg = Message::new("ping");
        apply_wrap_meta(&mut msg, None, true, Some("db"));
        assert!(msg.meta.is_empty(), "no resource ⇒ no meta written");
    }

    #[test]
    fn apply_wrap_meta_matches_message_builder_for_msg_path() {
        // The `call_service_with_msg` path applies WRAP meta onto a caller's
        // existing Message; it must produce the same WRAP keys as the
        // kind-based builder for an equivalent resource.
        let mut with_msg = Message::new("update");
        with_msg.set_meta(META_REQ_ACTION, "update");
        apply_wrap_meta(&mut with_msg, Some("acct"), true, Some("db"));

        let built = build_service_message("update", Some("acct"), true, Some("db"));

        for key in [
            META_REQ_ACTION,
            META_WRAP_RESOURCE,
            META_WRAP_ACCESS,
            META_WRAP_RESOURCE_TYPE,
        ] {
            assert_eq!(meta(&with_msg, key), meta(&built, key), "mismatch on {key}");
        }
    }
}
