//! Typed client wrappers around the streaming ABI. Each module per service
//! exposes both buffered (`do_request`, etc.) and streaming
//! (`do_request_stream`) helpers, mirroring the native client API names.

pub mod auth;
mod common;
pub mod config;
pub mod crypto;
pub mod database;
pub mod llm;
pub mod logger;
pub mod network;
pub mod storage;
pub mod vector;
