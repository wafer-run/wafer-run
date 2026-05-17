//! Unified service block implementations.
//!
//! Each block wraps an `Arc<dyn XService>` and delegates to the shared handler
//! in `wafer_core::interfaces`. Platform-specific code only provides the service
//! implementation; the block struct, `info()`, and message routing are shared.

/// Native auth service block wrapping an `Arc<dyn AuthService>`.
pub mod auth;
/// Native config service block wrapping an `Arc<dyn ConfigService>`.
pub mod config;
/// Native crypto service block wrapping an `Arc<dyn CryptoService>`.
pub mod crypto;
/// Native database service block wrapping an `Arc<dyn DatabaseService>`.
pub mod database;
/// Native image service block wrapping an `Arc<dyn ImageService>`.
pub mod image;
/// Native LLM service block wrapping an `Arc<dyn LlmService>`.
pub mod llm;
/// Native logger service block wrapping an `Arc<dyn LoggerService>`.
pub mod logger;
/// Native network service block wrapping an `Arc<dyn NetworkService>`.
pub mod network;
/// Native storage service block wrapping an `Arc<dyn StorageService>`.
pub mod storage;
/// Native vector service block wrapping a vector + embedding service pair.
pub mod vector;
