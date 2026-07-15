use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::stream::{self, BoxStream, Stream, StreamExt};
use tokio_util::sync::CancellationToken;

/// A byte-chunk stream with a paired cancellation token.
///
/// `InputStream` wraps an inner `Stream<Item = Vec<u8>>` and exposes it
/// via the standard `Stream` trait so consumers can use `StreamExt`
/// methods (`.next()`, `.collect()`, etc.). A `CancellationToken` is
/// always present — callers that own the upstream source can cancel it;
/// callers that only consume the stream can inspect it.
pub struct InputStream {
    inner: BoxStream<'static, Vec<u8>>,
    cancel: CancellationToken,
}

impl InputStream {
    /// An empty stream that yields no chunks.
    pub fn empty() -> Self {
        Self {
            inner: Box::pin(stream::empty()),
            cancel: CancellationToken::new(),
        }
    }

    /// A single-chunk stream wrapping the given byte vector.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            inner: Box::pin(stream::once(async move { bytes })),
            cancel: CancellationToken::new(),
        }
    }

    /// Wrap an arbitrary `Stream<Item = Vec<u8>>`.  A fresh
    /// `CancellationToken` is created; use [`from_stream_with_cancel`]
    /// to supply your own.
    pub fn from_stream<S>(stream: S) -> Self
    where
        S: Stream<Item = Vec<u8>> + Send + 'static,
    {
        Self {
            inner: Box::pin(stream),
            cancel: CancellationToken::new(),
        }
    }

    /// Wrap a stream together with a caller-supplied cancellation token.
    pub fn from_stream_with_cancel<S>(stream: S, cancel: CancellationToken) -> Self
    where
        S: Stream<Item = Vec<u8>> + Send + 'static,
    {
        Self {
            inner: Box::pin(stream),
            cancel,
        }
    }

    /// Return a reference to the paired cancellation token.
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Consume the stream, concatenating all chunks into a single `Vec<u8>`.
    ///
    /// The first chunk is moved rather than copied — single-chunk streams
    /// ([`from_bytes`](Self::from_bytes), the flow executor's shared-body
    /// view) are the common case on the dispatch hot path, and for them
    /// collection is copy-free (PERF-03).
    pub async fn collect_to_bytes(mut self) -> Vec<u8> {
        let Some(mut out) = self.inner.next().await else {
            return Vec::new();
        };
        while let Some(chunk) = self.inner.next().await {
            out.extend_from_slice(&chunk);
        }
        out
    }
}

impl Stream for InputStream {
    type Item = Vec<u8>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

#[cfg(test)]
mod tests {
    use futures::stream::{self, StreamExt};

    use super::*;

    #[tokio::test]
    async fn empty_stream_yields_no_bytes() {
        let mut s = InputStream::empty();
        assert!(s.next().await.is_none());
    }

    #[tokio::test]
    async fn from_bytes_yields_single_chunk() {
        let mut s = InputStream::from_bytes(b"hello".to_vec());
        let chunk = s.next().await;
        assert_eq!(chunk, Some(b"hello".to_vec()));
        assert!(s.next().await.is_none());
    }

    #[tokio::test]
    async fn from_stream_forwards_chunks() {
        let upstream = stream::iter(vec![vec![1u8], vec![2, 3], vec![4]]);
        let s = InputStream::from_stream(upstream);
        let chunks: Vec<_> = s.collect().await;
        assert_eq!(chunks, vec![vec![1], vec![2, 3], vec![4]]);
    }

    #[tokio::test]
    async fn collect_to_bytes_concatenates() {
        let s = InputStream::from_stream(stream::iter(vec![vec![1u8, 2], vec![3], vec![4, 5]]));
        let all = s.collect_to_bytes().await;
        assert_eq!(all, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn cancel_token_is_present() {
        let s = InputStream::empty();
        let _: &tokio_util::sync::CancellationToken = s.cancel_token();
    }
}
