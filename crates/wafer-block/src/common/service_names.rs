/// Service name constants for WAFER. Used as the `block` segment of the
/// service-op string and as alias keys registered by `solobase-core`.
pub struct ServiceName;

impl ServiceName {
    /// Database service (CRUD + raw SQL).
    pub const DATABASE: &str = "database";
    /// Object storage service.
    pub const STORAGE: &str = "storage";
    /// Cryptography service (hashing, signing, RNG).
    pub const CRYPTO: &str = "crypto";
    /// Outbound network service.
    pub const NETWORK: &str = "network";
    /// Structured-log emission service.
    pub const LOGGER: &str = "logger";
    /// Block configuration read/write service.
    pub const CONFIG: &str = "config";
    /// Host runtime utility service.
    pub const RUNTIME: &str = "runtime";
    /// Vector index service.
    pub const VECTOR: &str = "vector";
    /// Text embedding service.
    pub const EMBEDDING: &str = "embedding";
    /// Large-language-model chat/inference service.
    pub const LLM: &str = "llm";
}

/// Service operation constants for WAFER. Each constant is the canonical
/// `service.op` string used in `Message::kind`, WRAP grants, and capability
/// lookups.
pub struct ServiceOp;

impl ServiceOp {
    /// Fetch a single record by primary key.
    pub const DATABASE_GET: &str = "database.get";
    /// List records (with optional filter/sort/pagination).
    pub const DATABASE_LIST: &str = "database.list";
    /// Insert a new record.
    pub const DATABASE_CREATE: &str = "database.create";
    /// Update an existing record by primary key.
    pub const DATABASE_UPDATE: &str = "database.update";
    /// Delete a record by primary key.
    pub const DATABASE_DELETE: &str = "database.delete";
    /// Count records matching a filter.
    pub const DATABASE_COUNT: &str = "database.count";
    /// Execute a raw SELECT and return rows (cap-gated).
    pub const DATABASE_QUERY_RAW: &str = "database.query_raw";
    /// Execute a raw mutation statement (cap-gated).
    pub const DATABASE_EXEC_RAW: &str = "database.exec_raw";
    /// Aggregate sum of a numeric column.
    pub const DATABASE_SUM: &str = "database.sum";
    /// Delete every row matching a filter.
    pub const DATABASE_DELETE_WHERE: &str = "database.delete_where";
    /// Delete every row matching a filter and return the affected count.
    pub const DATABASE_DELETE_WHERE_COUNT: &str = "database.delete_where_count";
    /// Atomically remove and return rows matching a filter.
    pub const DATABASE_TAKE_WHERE: &str = "database.take_where";
    /// Update every row matching a filter.
    pub const DATABASE_UPDATE_WHERE: &str = "database.update_where";
    /// Atomically increment a numeric column on rows matching a filter.
    pub const DATABASE_INCREMENT_FIELD_WHERE: &str = "database.increment_field_where";
    /// Write an object to storage.
    pub const STORAGE_PUT: &str = "storage.put";
    /// Read an object from storage.
    pub const STORAGE_GET: &str = "storage.get";
    /// Delete an object from storage.
    pub const STORAGE_DELETE: &str = "storage.delete";
    /// List objects in a folder.
    pub const STORAGE_LIST: &str = "storage.list";
    /// Create a folder.
    pub const STORAGE_CREATE_FOLDER: &str = "storage.create_folder";
    /// Delete a folder.
    pub const STORAGE_DELETE_FOLDER: &str = "storage.delete_folder";
    /// List subfolders within a folder.
    pub const STORAGE_LIST_FOLDERS: &str = "storage.list_folders";
    /// Compute a password/string hash.
    pub const CRYPTO_HASH: &str = "crypto.hash";
    /// Verify a hash against a candidate value.
    pub const CRYPTO_COMPARE_HASH: &str = "crypto.compare_hash";
    /// Sign bytes with a named key.
    pub const CRYPTO_SIGN: &str = "crypto.sign";
    /// Verify a signature with a named key.
    pub const CRYPTO_VERIFY: &str = "crypto.verify";
    /// Generate cryptographically secure random bytes.
    pub const CRYPTO_RANDOM_BYTES: &str = "crypto.random_bytes";
    /// Perform an outbound HTTP request.
    pub const NETWORK_DO_REQUEST: &str = "network.do";
    /// Emit a debug-level log line.
    pub const LOGGER_DEBUG: &str = "logger.debug";
    /// Emit an info-level log line.
    pub const LOGGER_INFO: &str = "logger.info";
    /// Emit a warn-level log line.
    pub const LOGGER_WARN: &str = "logger.warn";
    /// Emit an error-level log line.
    pub const LOGGER_ERROR: &str = "logger.error";
    /// Read a config value.
    pub const CONFIG_GET: &str = "config.get";
    /// Write a config value.
    pub const CONFIG_SET: &str = "config.set";
    /// Create a vector index.
    pub const VECTOR_CREATE_INDEX: &str = "vector.create_index";
    /// Delete a vector index.
    pub const VECTOR_DELETE_INDEX: &str = "vector.delete_index";
    /// Upsert vectors into an index.
    pub const VECTOR_UPSERT: &str = "vector.upsert";
    /// Query nearest neighbors in an index.
    pub const VECTOR_QUERY: &str = "vector.query";
    /// Delete vectors by id.
    pub const VECTOR_DELETE: &str = "vector.delete";
    /// Count vectors in an index.
    pub const VECTOR_COUNT: &str = "vector.count";
    /// Compute text embeddings.
    pub const EMBEDDING_EMBED: &str = "embedding.embed";
    /// Count tokens for a piece of text.
    pub const EMBEDDING_COUNT_TOKENS: &str = "embedding.count_tokens";
    /// Run an LLM chat completion.
    pub const LLM_CHAT: &str = "llm.chat";
    /// List available LLM models.
    pub const LLM_LIST_MODELS: &str = "llm.list_models";
    /// Get the loaded-model / readiness status.
    pub const LLM_STATUS: &str = "llm.status";
    /// Load an LLM model into memory.
    pub const LLM_LOAD_MODEL: &str = "llm.load_model";
    /// Unload an LLM model from memory.
    pub const LLM_UNLOAD_MODEL: &str = "llm.unload_model";
    /// Generate an image from a prompt.
    pub const IMAGE_GENERATE: &str = "image.generate";
    /// List available image-generation models.
    pub const IMAGE_LIST_MODELS: &str = "image.list_models";
    /// Get the image-generation service status.
    pub const IMAGE_STATUS: &str = "image.status";
    /// Load an image-generation model into memory.
    pub const IMAGE_LOAD_MODEL: &str = "image.load_model";
    /// Unload an image-generation model from memory.
    pub const IMAGE_UNLOAD_MODEL: &str = "image.unload_model";
    /// Require an authenticated user; fail otherwise.
    pub const AUTH_REQUIRE_USER: &str = "auth.require_user";
    /// Require a valid bearer token; fail otherwise.
    pub const AUTH_REQUIRE_TOKEN: &str = "auth.require_token";
    /// Require the caller to have the specified role.
    pub const AUTH_REQUIRE_ROLE: &str = "auth.require_role";
    /// Fetch the authenticated user's profile.
    pub const AUTH_USER_PROFILE: &str = "auth.user_profile";
}
