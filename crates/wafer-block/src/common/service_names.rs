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
    /// Aggregate sum of a numeric column.
    pub const DATABASE_SUM: &str = "database.sum";
    /// Execute a raw SELECT and return rows (cap-gated).
    pub const DATABASE_QUERY_RAW: &str = "database.query_raw";
    /// Typed query: WRAP-authorized against the Statement's `collection`.
    pub const DATABASE_QUERY: &str = "database.query";
    /// Execute a raw mutation statement (cap-gated).
    pub const DATABASE_EXEC_RAW: &str = "database.exec_raw";
    /// Execute a raw DDL statement (cap-gated). Distinct from
    /// `DATABASE_EXEC_RAW` so the DDL sentinel is host-authoritative — set by
    /// the op arm the host dispatches on, not a forgeable request meta.
    pub const DATABASE_DDL: &str = "database.ddl";
    /// Typed execute: WRAP-authorized against the Statement's `collection`.
    pub const DATABASE_EXECUTE: &str = "database.execute";
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

    // -----------------------------------------------------------------------
    // Op-family slices — the canonical per-service op lists.
    //
    // These are the single source of truth for the interface action catalogs
    // in `crate::interfaces` (and through them the dispatcher's action
    // validation in `wafer-run`). Adding a new op constant to one of these
    // families requires adding it to the family slice AND declaring an
    // `ActionSpec` for it in the matching `*_action_spec` fn in
    // `crate::interfaces`; the drift tests there enforce slice ↔ catalog
    // agreement. Families without a well-known interface catalog (vector,
    // embedding, llm, image, auth) intentionally have no slice yet.
    // -----------------------------------------------------------------------

    /// Every `database.*` op — drives the `database@v1` action catalog in
    /// [`crate::interfaces::database_v1`].
    pub const DATABASE_OPS: &[&str] = &[
        Self::DATABASE_GET,
        Self::DATABASE_LIST,
        Self::DATABASE_CREATE,
        Self::DATABASE_UPDATE,
        Self::DATABASE_UPDATE_WHERE,
        Self::DATABASE_DELETE,
        Self::DATABASE_DELETE_WHERE,
        Self::DATABASE_DELETE_WHERE_COUNT,
        Self::DATABASE_TAKE_WHERE,
        Self::DATABASE_COUNT,
        Self::DATABASE_SUM,
        Self::DATABASE_INCREMENT_FIELD_WHERE,
        Self::DATABASE_QUERY,
        Self::DATABASE_QUERY_RAW,
        Self::DATABASE_EXECUTE,
        Self::DATABASE_EXEC_RAW,
        Self::DATABASE_DDL,
    ];

    /// Every `storage.*` op — drives the `storage@v1` action catalog in
    /// [`crate::interfaces::storage_v1`].
    pub const STORAGE_OPS: &[&str] = &[
        Self::STORAGE_PUT,
        Self::STORAGE_GET,
        Self::STORAGE_DELETE,
        Self::STORAGE_LIST,
        Self::STORAGE_CREATE_FOLDER,
        Self::STORAGE_DELETE_FOLDER,
        Self::STORAGE_LIST_FOLDERS,
    ];

    /// Every `crypto.*` op — drives the `crypto@v1` action catalog in
    /// [`crate::interfaces::crypto_v1`].
    pub const CRYPTO_OPS: &[&str] = &[
        Self::CRYPTO_HASH,
        Self::CRYPTO_COMPARE_HASH,
        Self::CRYPTO_SIGN,
        Self::CRYPTO_VERIFY,
        Self::CRYPTO_RANDOM_BYTES,
    ];

    /// Every `network.*` op — drives the `http-client@v1` action catalog in
    /// [`crate::interfaces::http_client_v1`].
    pub const NETWORK_OPS: &[&str] = &[Self::NETWORK_DO_REQUEST];

    /// Every `logger.*` op — drives the `logger@v1` action catalog in
    /// [`crate::interfaces::logger_v1`].
    pub const LOGGER_OPS: &[&str] = &[
        Self::LOGGER_DEBUG,
        Self::LOGGER_INFO,
        Self::LOGGER_WARN,
        Self::LOGGER_ERROR,
    ];

    /// Every `config.*` op — drives the `config@v1` action catalog in
    /// [`crate::interfaces::config_v1`].
    pub const CONFIG_OPS: &[&str] = &[Self::CONFIG_GET, Self::CONFIG_SET];
}
