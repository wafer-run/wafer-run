//! Core (non-streaming) guest ABI: version negotiation and frame types
//! shared by the host runtime and the guest SDK (PERF-01).
//!
//! Two wire formats exist:
//!
//! - **v1 (implicit)** — JSON. A guest without a `__wafer_abi_version`
//!   export speaks v1: the host sends `__wafer_handle` a JSON 2-tuple
//!   `[Message, [byte, byte, ...]]` and decodes a JSON [`GuestResult`]
//!   back. Bodies serialize as integer arrays (~3.4 chars/byte — the
//!   PERF-01 finding); v1 support never goes away, so already-built wasm
//!   artifacts keep working unmodified.
//! - **v2** — MessagePack via [`crate::codec`] (named-map structs, the
//!   same forward-compatibility policy as the wire types) with bodies as
//!   native bin byte strings (`serde_bytes`), i.e. a memcpy instead of a
//!   per-byte integer encoding. A guest advertises v2 by exporting
//!   `extern "C" fn __wafer_abi_version() -> i32` returning
//!   [`ABI_VERSION`]. The `#[wafer_block]` macro emits this export
//!   automatically, so rebuilding a block against the current SDK flips
//!   it to v2 with zero source changes.
//!
//! Both sides use the SAME types below for both codecs — `serde_bytes`
//! fields encode as integer arrays under JSON and as bin under
//! MessagePack, so there is no per-version type surface to drift.

use serde::{Deserialize, Serialize};

use crate::core_types::{Message, MetaEntry, WaferError};

/// The newest core-ABI version this crate speaks. Guests report it via the
/// generated `__wafer_abi_version` export; hosts reject anything newer
/// (fail loud, never silently downgrade a future guest).
pub const ABI_VERSION: i32 = 2;

/// Name of the guest export carrying the ABI version. Absence means v1.
pub const ABI_VERSION_EXPORT: &str = "__wafer_abi_version";

/// Optional guest export negotiating the *host-call* payload codec:
/// `fn __wafer_host_codec() -> i32`. Absent = MessagePack (every SDK guest);
/// `HOST_CODEC_JSON` = the guest writes JSON request bodies and reads JSON
/// response frames and errors, and the host transcodes. Independent of
/// `__wafer_abi_version`, which negotiates only the handle/lifecycle frames.
///
/// # What a JSON guest cannot do
///
/// The host transcodes the WHOLE request body of a host call, so a JSON guest
/// has **no raw request-body path**: every byte it writes with
/// `__wafer_host_stream_write_chunk` must parse as JSON. That rules out the
/// streaming-upload ops whose request frames are opaque application bytes
/// (`storage.put_streaming`'s body chunks) — the transcode fails and the call
/// resumes with the `InvalidArgument` sentinel. Response direction is
/// different: a handler can mark frames raw
/// (`wafer_block::stream::raw_frames_marker`) and the host forwards them
/// verbatim, which is how `storage.get` returns an object body to a JSON
/// guest. Attachments (`__wafer_host_stream_attach`) and attachment lookup
/// stay MessagePack-only and likewise answer `InvalidArgument`.
pub const HOST_CODEC_EXPORT: &str = "__wafer_host_codec";
/// `__wafer_host_codec` return value meaning the guest speaks JSON host-call payloads.
pub const HOST_CODEC_JSON: i32 = 1;
/// `__wafer_host_codec` return value meaning the guest speaks MessagePack host-call
/// payloads — the same as the implicit default when the export is absent.
pub const HOST_CODEC_RMP: i32 = 2;

/// Host→guest `__wafer_handle` frame, borrowed for encoding. Serializes as
/// a 2-tuple, matching the v1 wire shape (`[Message, body]`) so one type
/// serves both codecs.
#[derive(Serialize)]
pub struct CallFrameRef<'a>(pub &'a Message, #[serde(with = "serde_bytes")] pub &'a [u8]);

/// Owned counterpart of [`CallFrameRef`] for guest-side decoding.
#[derive(Deserialize)]
pub struct CallFrame(pub Message, pub serde_bytes::ByteBuf);

/// Which arm of a [`GuestResult`] the host should take.
///
/// # Wire format
///
/// This is (de)serialized as the exact discriminator **string** —
/// `"Respond"`, `"Error"`, `"Drop"`, `"Continue"` — on BOTH the v1 (JSON) and
/// v2 (MessagePack) codecs, byte-for-byte identical to the old `action:
/// String` field it replaced. The serde impls are hand-written for this
/// reason: a derived enum would serialize as a variant *index* under
/// `rmp-serde`, silently breaking the v2 wire. Do not switch to a derive.
///
/// An unrecognized discriminator is a decode error (the host previously
/// surfaced it as an `Internal` "unknown action" error one step later; the
/// message is preserved on the deserialize side).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestAction {
    /// Return a response body (and optional trailing meta / continuation).
    Respond,
    /// Return a structured error.
    Error,
    /// Drop the request with no response.
    Drop,
    /// Forward an updated message to the next flow step.
    Continue,
}

impl GuestAction {
    /// The exact wire discriminator string.
    pub fn as_wire(self) -> &'static str {
        match self {
            GuestAction::Respond => "Respond",
            GuestAction::Error => "Error",
            GuestAction::Drop => "Drop",
            GuestAction::Continue => "Continue",
        }
    }

    /// Parse a wire discriminator, or `None` if unrecognized.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "Respond" => Some(GuestAction::Respond),
            "Error" => Some(GuestAction::Error),
            "Drop" => Some(GuestAction::Drop),
            "Continue" => Some(GuestAction::Continue),
            _ => None,
        }
    }
}

impl Serialize for GuestAction {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // Emit a plain string on every codec — identical bytes to the former
        // `action: String` field (a derived enum would be an index in rmp).
        s.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for GuestAction {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        GuestAction::from_wire(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown guest action {s:?}")))
    }
}

/// The result returned by a WASM block's `handle` function.
///
/// Serialized back to the host with the negotiated codec; the host maps it
/// to an `OutputStream`. Use [`GuestResult::respond`], [`GuestResult::error`],
/// or [`GuestResult::drop_request`] to construct values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestResult {
    /// Which arm the host should take. Serialized as its wire string
    /// (`"Respond"`/`"Error"`/`"Drop"`/`"Continue"`) — see [`GuestAction`].
    pub action: GuestAction,
    /// Response body and trailing meta when `action == Respond`.
    pub response: Option<GuestResponse>,
    /// Structured error when `action == Error`.
    pub error: Option<WaferError>,
    /// Optional updated `Message` to forward downstream alongside the response.
    pub message: Option<Message>,
}

/// The response body returned by a successful WASM block invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestResponse {
    /// Body bytes returned to the caller. `serde_bytes` keeps this a native
    /// byte string under MessagePack (and a plain integer array under JSON).
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    /// Trailing meta entries appended to the response stream.
    #[serde(default)]
    pub meta: Vec<MetaEntry>,
}

impl GuestResult {
    /// Respond with a body (and no trailing meta).
    pub fn respond(data: Vec<u8>) -> Self {
        Self {
            action: GuestAction::Respond,
            response: Some(GuestResponse { data, meta: vec![] }),
            error: None,
            message: None,
        }
    }

    /// Respond with body and meta entries.
    pub fn respond_with_meta(data: Vec<u8>, meta: Vec<MetaEntry>) -> Self {
        Self {
            action: GuestAction::Respond,
            response: Some(GuestResponse { data, meta }),
            error: None,
            message: None,
        }
    }

    /// Return an error to the caller.
    pub fn error(err: WaferError) -> Self {
        Self {
            action: GuestAction::Error,
            response: None,
            error: Some(err),
            message: None,
        }
    }

    /// Drop the request (no response, no error).
    pub fn drop_request() -> Self {
        Self {
            action: GuestAction::Drop,
            response: None,
            error: None,
            message: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg() -> Message {
        Message::new("bench.echo")
    }

    /// v2 frames carry the body as a MessagePack bin byte string: encoded
    /// size must be body-length + small constant overhead, never the
    /// ~3.4x integer-array blowup. This pins the `serde_bytes` wiring —
    /// a plain `Vec<u8>` field would silently regress to an array.
    #[test]
    fn v2_frame_encodes_body_as_bin() {
        let body = vec![0xABu8; 64 * 1024];
        let m = msg();
        let encoded = crate::codec::encode(&CallFrameRef(&m, &body)).unwrap();
        assert!(
            encoded.len() < body.len() + 1024,
            "64 KiB body must encode near-1:1 (bin), got {} bytes",
            encoded.len()
        );
        let decoded: CallFrame = crate::codec::decode(&encoded).unwrap();
        assert_eq!(decoded.0.kind, "bench.echo");
        assert_eq!(decoded.1.as_slice(), body.as_slice());
    }

    /// v2 guest results carry response data as bin, and round-trip.
    #[test]
    fn v2_guest_result_roundtrip_data_as_bin() {
        let result = GuestResult::respond_with_meta(
            vec![7u8; 4096],
            vec![MetaEntry {
                key: "content-type".into(),
                value: "application/octet-stream".into(),
            }],
        );
        let encoded = crate::codec::encode(&result).unwrap();
        assert!(
            encoded.len() < 4096 + 512,
            "4 KiB data must encode near-1:1 (bin), got {} bytes",
            encoded.len()
        );
        let decoded: GuestResult = crate::codec::decode(&encoded).unwrap();
        assert_eq!(decoded.action, GuestAction::Respond);
        let resp = decoded.response.unwrap();
        assert_eq!(resp.data, vec![7u8; 4096]);
        assert_eq!(resp.meta.len(), 1);
    }

    /// The SAME types must stay decodable from the v1 JSON wire shape —
    /// the host uses them to decode results from old (JSON-ABI) wasm
    /// artifacts, where `data` arrives as an integer array.
    #[test]
    fn shared_types_decode_v1_json_shapes() {
        let m = msg();
        let body: &[u8] = &[1, 2, 3];
        let json = serde_json::to_vec(&CallFrameRef(&m, body)).unwrap();
        let decoded: CallFrame = serde_json::from_slice(&json).unwrap();
        assert_eq!(decoded.1.as_slice(), body);

        let v1_result = serde_json::json!({
            "action": "Respond",
            "response": { "data": [1, 2, 3], "meta": [] },
            "error": null,
            "message": null,
        });
        let decoded: GuestResult = serde_json::from_value(v1_result).unwrap();
        assert_eq!(decoded.response.unwrap().data, vec![1, 2, 3]);
    }

    /// The load-bearing invariant for the `action: String` → [`GuestAction`]
    /// change: the discriminator must stay a **string** on BOTH codecs, so the
    /// wire is byte-identical to the old field and no wasm artifact needs a
    /// rebuild. A derived enum would encode as a variant index under
    /// `rmp-serde` and silently break v2 — this test would fail if the custom
    /// serde impls were ever replaced with a derive.
    #[test]
    fn guest_action_wire_stays_a_string_on_both_codecs() {
        #[derive(serde::Deserialize)]
        struct StringActionView {
            action: String,
        }
        for action in [
            GuestAction::Respond,
            GuestAction::Error,
            GuestAction::Drop,
            GuestAction::Continue,
        ] {
            let result = GuestResult {
                action,
                response: None,
                error: None,
                message: None,
            };
            // v2 (rmp): decoding `action` as a String only succeeds if it was
            // serialized as a msgpack str, not an int index.
            let rmp = crate::codec::encode(&result).unwrap();
            let via_rmp: StringActionView = crate::codec::decode(&rmp).unwrap();
            assert_eq!(via_rmp.action, action.as_wire(), "rmp action must be a str");
            // v1 (JSON): the action field is the plain string.
            let json = serde_json::to_value(&result).unwrap();
            assert_eq!(
                json["action"],
                action.as_wire(),
                "json action must be a str"
            );
        }
    }

    /// An unrecognized discriminator is a decode error (naming the bad value),
    /// not a silently-accepted variant.
    #[test]
    fn unknown_guest_action_is_a_decode_error() {
        let bad = serde_json::json!({
            "action": "Bogus", "response": null, "error": null, "message": null,
        });
        let err = serde_json::from_value::<GuestResult>(bad).unwrap_err();
        assert!(
            err.to_string().contains("Bogus"),
            "error must name it: {err}"
        );
    }
}
