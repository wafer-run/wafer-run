pub mod handler;
/// Multi-backend router that dispatches `llm.*` ops to the chosen backend.
pub mod router;
/// `LlmService` trait plus chat/completion/embedding request and response types.
pub mod service;
