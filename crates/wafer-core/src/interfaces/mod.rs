pub(crate) mod handler_util;

/// Auth interface: `AuthService` trait + the `auth.*` message handler.
pub mod auth;
/// Config interface: `ConfigService` trait + the `config.*` message handler.
pub mod config;
/// Crypto interface: `CryptoService` trait + the `crypto.*` message handler.
pub mod crypto;
/// Database interface: `DatabaseService` trait, schema/filter types, and the `db.*` handler.
pub mod database;
/// Image interface: `ImageService` trait + the `image.*` message handler.
pub mod image;
/// LLM interface: `LlmService` trait + the `llm.*` message handler.
pub mod llm;
/// Logger interface: `LoggerService` trait + the `log.*` message handler.
pub mod logger;
/// Network interface: `NetworkService` trait + the `network.*` message handler.
pub mod network;
/// Storage interface: `StorageService` trait, object types, and the `storage.*` handler.
pub mod storage;
/// Vector + embedding interfaces, plus catalog/RRF helpers and the dispatch handler.
pub mod vector;
