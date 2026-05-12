//! Image-generation service abstraction.
//!
//! Mirrors `interfaces::llm` in layout. The trait is in `service`; the
//! `claims_backend`-dispatching multi-backend router is in `router`; the
//! block handler that decodes wire frames and forwards to a service impl
//! is in `handler`.

pub mod handler;
pub mod router;
pub mod service;
