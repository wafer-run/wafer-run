//! Compile fixture for the `wasm32` arm of `InputStream`'s inner stream type.
//! See this crate's `Cargo.toml` for why it exists and how it is built.
//!
//! Everything below is written the way a Cloudflare Workers or browser
//! service-worker adapter writes it: take the request body as it arrives, wrap
//! it as an `InputStream`, and hand it on — without ever buffering it. The body
//! here is `!Send` for the same structural reason a real one is (an `Rc` stands
//! in for the `JsValue` inside `worker::ByteStream`), so this crate does not
//! compile at all if `InputStream` goes back to a `Send` inner stream.
//!
//! Nothing in here ever runs: `cargo build` is the whole test, and what it
//! checks is which bounds hold, not which bytes move.
//!
//! The native half of the same contract — that `InputStream` is still `Send`
//! where threads exist — is pinned by the `assert_send` const in
//! `crates/wafer-block/src/streams/input.rs`.

#[cfg(not(target_arch = "wasm32"))]
compile_error!(
    "this fixture only exercises the wasm32 arm of InputStream's inner stream type; \
     build it with --target wasm32-unknown-unknown (scripts/check.sh wasm does)"
);

use std::{
    cell::RefCell,
    collections::VecDeque,
    marker::PhantomData,
    pin::Pin,
    rc::Rc,
    task::{Context as TaskContext, Poll},
};

use futures::stream::Stream;
use wafer_block::{core_types::WaferError, Context, InputStream};

/// A request body that cannot leave the thread that made it.
///
/// The `Rc` is the point: a real body on this target is a `worker::ByteStream`
/// or a `wasm_streams::ReadableStream` `IntoStream`, both of which own a
/// `JsValue` tied to their isolate. Any of them makes the stream `!Send`, and
/// `!Send` is the only property of theirs this fixture needs.
pub struct JsLikeBody {
    chunks: Rc<RefCell<VecDeque<Vec<u8>>>>,
}

impl JsLikeBody {
    /// A body that yields `chunks` in order and then ends.
    pub fn new(chunks: Vec<Vec<u8>>) -> Self {
        Self {
            chunks: Rc::new(RefCell::new(chunks.into())),
        }
    }
}

impl Stream for JsLikeBody {
    type Item = Result<Vec<u8>, WaferError>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.chunks.borrow_mut().pop_front().map(Ok))
    }
}

/// Resolves `IS_SEND` to `true` when `T: Send` (the inherent impl below wins)
/// and to `false` otherwise (this fallback), which is how the assertions below
/// can state that something is *not* `Send`.
trait SendProbeFallback {
    const IS_SEND: bool = false;
}

struct SendProbe<T>(PhantomData<T>);

impl<T> SendProbeFallback for SendProbe<T> {}

impl<T: Send> SendProbe<T> {
    const IS_SEND: bool = true;
}

/// A `Send` body would make every assertion in this crate vacuous: the old
/// `S: Stream + Send` bound accepted those, so the fixture would pass against
/// precisely the code it exists to reject.
const _: () = assert!(
    !<SendProbe<JsLikeBody>>::IS_SEND,
    "JsLikeBody must stay !Send — it is the stand-in for a JS-backed body"
);

/// `InputStream` inherits the body's thread affinity on wasm32. Asserted so
/// the relaxation is visible as a property of the type and not only as
/// "the crate happened to compile".
const _: () = assert!(
    !<SendProbe<InputStream>>::IS_SEND,
    "InputStream wraps a LocalBoxStream on wasm32, so it cannot be Send there"
);

/// Positive control. A probe that answered `false` for everything would make
/// both assertions above hold for every type, including the `Send` ones they
/// are meant to exclude.
const _: () = assert!(
    <SendProbe<Vec<u8>>>::IS_SEND,
    "the probe must resolve to the inherent impl for a Send type"
);

/// What an adapter does with an inbound request body: wrap it, never buffer it.
pub fn wrap_request_body(chunks: Vec<Vec<u8>>) -> InputStream {
    InputStream::from_stream(JsLikeBody::new(chunks))
}

/// Holding the wrapped body across an await point, which is what any adapter
/// handler does with it. This is an example of the relaxed path rather than an
/// assertion about it: the resulting future is `!Send`, and nothing states so
/// here, because the future type is unnameable and the `SendProbe` below needs
/// a name. What it does pin is that `collect_to_bytes` is reachable from a
/// local body at all.
pub async fn collect_request_body(chunks: Vec<Vec<u8>>) -> Result<Vec<u8>, WaferError> {
    wrap_request_body(chunks).collect_to_bytes().await
}

/// A local body typechecks all the way into `storage.put_streaming`, which
/// re-frames it behind a header chunk via `from_stream_with_cancel`. That
/// re-wrap is the one place in the tree that rebuilds an `InputStream` out of
/// an existing one, so it is where a stray `Send` bound would strand a local
/// body halfway down the path.
///
/// Where the bytes go after that is the backend's choice and not this
/// fixture's business: `StorageService::put_streaming`'s trait default
/// collects the stream and forwards to `put`, so only an overriding backend
/// actually streams.
pub async fn upload_request_body(
    ctx: &dyn Context,
    folder: &str,
    key: &str,
    content_type: &str,
    body: InputStream,
) -> Result<(), WaferError> {
    wafer_core::clients::storage::put_stream(ctx, folder, key, content_type, body).await
}
