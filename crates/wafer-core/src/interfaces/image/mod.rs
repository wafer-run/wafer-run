//! Image-generation service abstraction.
//!
//! Mirrors `interfaces::llm` in layout. The trait is in `service`; the
//! `claims_backend`-dispatching multi-backend router is in `router`; the
//! block handler that decodes wire frames and forwards to a service impl
//! is in `handler`.

pub mod handler;
/// Multi-backend router that dispatches `image.*` ops based on `claims_backend`.
pub mod router;
/// `ImageService` trait and image transform / encode request/response types.
pub mod service;
