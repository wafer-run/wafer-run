//! Shared wire-format types. Consumed by `wafer_core` (host handlers + native
//! clients) and `wafer_sdk` (WASM skill clients). Encoded via `wafer_block::codec`
//! (currently MessagePack).
//!
//! Each service interface gets its own submodule. Types are pure DTOs —
//! `#[derive(Serialize, Deserialize, Debug, Clone)]` only, no behavior.
//!
//! # Forward-compatibility strategy
//!
//! Host and guest are versioned independently, so a newer peer must be able to
//! talk to an older one without a coordinated upgrade. Two mechanisms combine
//! to make that work, and both must be upheld when editing these types:
//!
//! 1. **Named-map encoding.** [`crate::codec::encode`] uses
//!    `rmp_serde::to_vec_named`, which serializes each struct as a MessagePack
//!    *map keyed by field name* rather than a positional array. Fields are
//!    therefore matched by name on decode, so adding, removing, or reordering
//!    fields does not shift any other field's value.
//! 2. **`#[serde(default)]` on every field a newer producer might omit.** When
//!    an older producer sends a payload that lacks a field a newer consumer
//!    expects, the named map simply won't contain that key; `#[serde(default)]`
//!    fills it with the type's default instead of failing the decode. Unknown
//!    *extra* fields (newer producer → older consumer) are ignored because
//!    `deny_unknown_fields` is never set.
//!
//! ## Rules for evolving a wire type
//!
//! - New fields MUST be additive and carry `#[serde(default)]` (or be an
//!   `Option`) so older producers that omit them still decode.
//! - Never repurpose or change the *type* of an existing field name — that
//!   silently corrupts old↔new traffic. Add a new field instead.
//! - Removing a field is safe for decoders (extra keys are ignored) but the
//!   meaning is lost; prefer deprecation over reuse.
//!
//! The `unknown_field_is_forward_compatible` test in [`crate::codec`] locks in
//! the extra-field tolerance.

pub mod auth;
pub mod config;
pub mod crypto;
pub mod database;
pub mod image;
pub mod llm;
pub mod logger;
pub mod network;
pub mod storage;
pub mod vector;
