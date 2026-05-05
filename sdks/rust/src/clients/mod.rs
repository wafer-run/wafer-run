//! Typed client wrappers around the streaming ABI. Each module per service
//! exposes both buffered (`do_request`, etc.) and streaming
//! (`do_request_stream`) helpers, mirroring the native client API names.

pub mod network;
// Subsequent services added in their own tasks.
