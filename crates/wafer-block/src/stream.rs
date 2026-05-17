//! [`StreamEvent`] — the event type yielded by an
//! [`crate::streams::output::OutputStream`]. Streams produce zero or more
//! non-terminal events (chunks/meta) followed by exactly one terminal event
//! (complete/error/drop/continue).

use crate::core_types::{Message, MetaEntry, WaferError};

/// An event in an OutputStream. The stream yields zero-or-more non-terminal
/// events followed by exactly one terminal event.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// Body bytes. Non-terminal. Zero or more per stream.
    Chunk(Vec<u8>),

    /// Mid-stream or trailing metadata. Non-terminal. Zero or more per stream.
    Meta(MetaEntry),

    /// Terminal: stream completed normally. Carries trailing metadata.
    Complete {
        /// Trailing metadata (e.g. HTTP trailers) emitted with the close.
        meta: Vec<MetaEntry>,
    },

    /// Terminal: stream failed.
    ///
    /// Boxed to keep the enum size closer to the smaller non-terminal variants
    /// (`Chunk` is 24 bytes; unboxed `WaferError` would be ~56 bytes).
    Error(Box<WaferError>),

    /// Terminal: block chose to drop the request (HTTP 204-equivalent).
    /// Valid only with no preceding Chunk or Meta events.
    Drop,

    /// Terminal: forward to another block instead of handling.
    /// Valid only with no preceding Chunk or Meta events.
    Continue(Message),
}

impl StreamEvent {
    /// Whether this event is a terminal (Complete/Error/Drop/Continue).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            StreamEvent::Complete { .. }
                | StreamEvent::Error(_)
                | StreamEvent::Drop
                | StreamEvent::Continue(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_types::{ErrorCode, Message, MetaEntry, WaferError};

    #[test]
    fn stream_event_variants_construct() {
        let _a = StreamEvent::Chunk(vec![1, 2, 3]);
        let _b = StreamEvent::Meta(MetaEntry {
            key: "Content-Type".into(),
            value: "text/event-stream".into(),
        });
        let _c = StreamEvent::Complete { meta: vec![] };
        let _d = StreamEvent::Error(Box::new(WaferError {
            code: ErrorCode::Unknown,
            message: "test error".into(),
            meta: vec![],
        }));
        let _e = StreamEvent::Drop;
        let _f = StreamEvent::Continue(Message {
            kind: "forward".into(),
            meta: vec![],
        });
    }

    #[test]
    fn chunk_equality() {
        assert_eq!(
            StreamEvent::Chunk(vec![1, 2, 3]),
            StreamEvent::Chunk(vec![1, 2, 3])
        );
    }

    #[test]
    fn terminal_classification() {
        assert!(!StreamEvent::Chunk(vec![]).is_terminal());
        assert!(!StreamEvent::Meta(MetaEntry {
            key: "k".into(),
            value: "v".into()
        })
        .is_terminal());
        assert!(StreamEvent::Complete { meta: vec![] }.is_terminal());
        assert!(StreamEvent::Error(Box::new(WaferError {
            code: ErrorCode::Unknown,
            message: "x".into(),
            meta: vec![],
        }))
        .is_terminal());
        assert!(StreamEvent::Drop.is_terminal());
    }
}
