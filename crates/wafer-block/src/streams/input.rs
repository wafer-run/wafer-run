use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{
    ready,
    stream::{self, Stream, StreamExt},
};
use tokio_util::sync::CancellationToken;

use crate::{compat::MaybeSend, core_types::WaferError};

/// The boxed stream an [`InputStream`] holds.
///
/// Native: `BoxStream`, i.e. `Pin<Box<dyn Stream + Send>>`. A body can be
/// produced on one task and consumed on another, so the inner stream has to
/// cross threads.
///
/// `wasm32`: `LocalBoxStream`, i.e. `Pin<Box<dyn Stream>>`. Every JS-backed
/// byte stream on that target is `!Send` — `worker::ByteStream` and
/// `wasm_streams`' `IntoStream` both hold a `JsValue`, which belongs to the
/// isolate that created it. A `Send` inner type therefore leaves a Cloudflare
/// Workers or browser service-worker adapter with no way to hand a request
/// body to a block as a stream at all: it has to buffer the whole body first
/// and cap how big that buffer may get. wasm32 is single-threaded and the
/// rest of the block ABI already drops `Send` there ([`crate::compat::MaybeSend`],
/// `#[wafer_async_trait]`, [`crate::spawn::spawn_producer`]), so matching it
/// here costs nothing that target ever had.
#[cfg(not(target_arch = "wasm32"))]
type BoxedByteStream = stream::BoxStream<'static, Result<Vec<u8>, WaferError>>;
#[cfg(target_arch = "wasm32")]
type BoxedByteStream = stream::LocalBoxStream<'static, Result<Vec<u8>, WaferError>>;

/// A byte-chunk stream with a paired cancellation token.
///
/// Each item is `Ok(chunk)` or `Err(error)`. The stream ends in one of two
/// ways, and a consumer must tell them apart:
///
/// - `None` after only `Ok` items: the body arrived whole.
/// - An `Err` item: the body did NOT arrive whole (the connection dropped,
///   the transport's size cap or read deadline was hit, the producer
///   aborted). Every chunk before it is a truncated prefix, so a consumer
///   must not commit, store or act on it as if it were the whole body. The
///   `Err` is terminal: every poll after it returns `None`.
///
/// `InputStream` exposes the items via the standard `Stream` trait so
/// consumers can use `StreamExt` methods (`.next()`, `.try_collect()`,
/// etc.); [`collect_to_bytes`](Self::collect_to_bytes) collects the whole
/// body or returns the error. A `CancellationToken` is always present —
/// callers that own the upstream source can cancel it; callers that only
/// consume the stream can inspect it.
///
/// The type is `Send` on native and `!Send` on wasm32 — see the
/// `BoxedByteStream` alias above for why.
pub struct InputStream {
    inner: BoxedByteStream,
    cancel: CancellationToken,
    /// Set once an `Err` item has been yielded; every later poll is `None`.
    failed: bool,
}

/// Native `InputStream`s cross threads, and the whole dispatch path assumes
/// it: this pins that the wasm32 relaxation above did not leak into the
/// native build.
#[cfg(not(target_arch = "wasm32"))]
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<InputStream>();
};

impl InputStream {
    /// An empty stream that yields no chunks.
    pub fn empty() -> Self {
        Self::from_boxed(Box::pin(stream::empty()), CancellationToken::new())
    }

    /// A single-chunk stream wrapping the given byte vector.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self::from_boxed(
            Box::pin(stream::once(async move { Ok(bytes) })),
            CancellationToken::new(),
        )
    }

    /// Wrap an arbitrary stream of chunk results. A fresh
    /// `CancellationToken` is created; use [`from_stream_with_cancel`]
    /// to supply your own.
    ///
    /// The source maps its own read failures to `Err`: a transport that
    /// yields `Result<Bytes, E>` passes the error on, never an empty chunk
    /// in its place, or a truncated body reads as a complete one.
    ///
    /// [`MaybeSend`] is `Send` on native and vacuous on wasm32, so a JS-backed
    /// request body (`worker::ByteStream`, `wasm_streams`' `IntoStream`) can be
    /// wrapped as-is there instead of being buffered — see the `BoxedByteStream`
    /// alias in this module.
    ///
    /// [`from_stream_with_cancel`]: Self::from_stream_with_cancel
    pub fn from_stream<S>(stream: S) -> Self
    where
        S: Stream<Item = Result<Vec<u8>, WaferError>> + MaybeSend + 'static,
    {
        Self::from_boxed(Box::pin(stream), CancellationToken::new())
    }

    /// Wrap a stream together with a caller-supplied cancellation token.
    ///
    /// Same item type and [`MaybeSend`] bound as
    /// [`from_stream`](Self::from_stream).
    pub fn from_stream_with_cancel<S>(stream: S, cancel: CancellationToken) -> Self
    where
        S: Stream<Item = Result<Vec<u8>, WaferError>> + MaybeSend + 'static,
    {
        Self::from_boxed(Box::pin(stream), cancel)
    }

    fn from_boxed(inner: BoxedByteStream, cancel: CancellationToken) -> Self {
        Self {
            inner,
            cancel,
            failed: false,
        }
    }

    /// Return a reference to the paired cancellation token.
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Consume the stream, concatenating all chunks into a single `Vec<u8>`,
    /// or return the stream's error if the body did not arrive whole. The
    /// chunks read before the error are discarded.
    ///
    /// The first chunk is moved rather than copied — single-chunk streams
    /// ([`from_bytes`](Self::from_bytes), the flow executor's shared-body
    /// view) are the common case on the dispatch hot path, and for them
    /// collection is copy-free (PERF-03).
    pub async fn collect_to_bytes(mut self) -> Result<Vec<u8>, WaferError> {
        let Some(first) = self.next().await else {
            return Ok(Vec::new());
        };
        let mut out = first?;
        while let Some(chunk) = self.next().await {
            out.extend_from_slice(&chunk?);
        }
        Ok(out)
    }
}

impl Stream for InputStream {
    type Item = Result<Vec<u8>, WaferError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.failed {
            return Poll::Ready(None);
        }
        let item = ready!(Pin::new(&mut self.inner).poll_next(cx));
        if matches!(item, Some(Err(_))) {
            self.failed = true;
        }
        Poll::Ready(item)
    }
}

#[cfg(test)]
mod tests {
    use futures::stream::{self, StreamExt};

    use super::*;
    use crate::core_types::ErrorCode;

    fn aborted() -> WaferError {
        WaferError::new(ErrorCode::Unavailable, "connection reset")
    }

    #[tokio::test]
    async fn empty_stream_yields_no_bytes() {
        let mut s = InputStream::empty();
        assert!(s.next().await.is_none());
    }

    #[tokio::test]
    async fn from_bytes_yields_single_chunk() {
        let mut s = InputStream::from_bytes(b"hello".to_vec());
        let chunk = s.next().await;
        assert_eq!(chunk, Some(Ok(b"hello".to_vec())));
        assert!(s.next().await.is_none());
    }

    #[tokio::test]
    async fn from_stream_forwards_chunks() {
        let upstream = stream::iter(vec![Ok(vec![1u8]), Ok(vec![2, 3]), Ok(vec![4])]);
        let s = InputStream::from_stream(upstream);
        let chunks: Vec<_> = s.collect().await;
        assert_eq!(chunks, vec![Ok(vec![1]), Ok(vec![2, 3]), Ok(vec![4])]);
    }

    #[tokio::test]
    async fn collect_to_bytes_concatenates() {
        let s = InputStream::from_stream(stream::iter(vec![
            Ok(vec![1u8, 2]),
            Ok(vec![3]),
            Ok(vec![4, 5]),
        ]));
        let all = s.collect_to_bytes().await.expect("a whole body collects");
        assert_eq!(all, vec![1, 2, 3, 4, 5]);
    }

    /// A body that fails after some chunks is not a shorter body.
    #[tokio::test]
    async fn collect_to_bytes_returns_the_failure_not_the_prefix() {
        let s = InputStream::from_stream(stream::iter(vec![Ok(vec![1u8, 2]), Err(aborted())]));
        assert_eq!(s.collect_to_bytes().await, Err(aborted()));
    }

    #[tokio::test]
    async fn collect_to_bytes_returns_a_failure_before_any_chunk() {
        let s = InputStream::from_stream(stream::iter(vec![Err(aborted())]));
        assert_eq!(s.collect_to_bytes().await, Err(aborted()));
    }

    /// The failure is terminal even if the source keeps producing after it,
    /// so a consumer that skipped over the `Err` still cannot read on as if
    /// the body continued.
    #[tokio::test]
    async fn failure_is_terminal() {
        let mut s = InputStream::from_stream(stream::iter(vec![
            Ok(vec![1u8]),
            Err(aborted()),
            Ok(vec![2]),
        ]));
        assert_eq!(s.next().await, Some(Ok(vec![1])));
        assert_eq!(s.next().await, Some(Err(aborted())));
        assert_eq!(s.next().await, None);
        assert_eq!(s.next().await, None);
    }

    #[tokio::test]
    async fn cancel_token_is_present() {
        let s = InputStream::empty();
        let _: &tokio_util::sync::CancellationToken = s.cancel_token();
    }
}
