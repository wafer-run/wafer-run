/// Service name constants for WAFER.
pub struct ServiceName;

impl ServiceName {
    pub const DATABASE: &str = "database";
    pub const STORAGE: &str = "storage";
    pub const CRYPTO: &str = "crypto";
    pub const NETWORK: &str = "network";
    pub const LOGGER: &str = "logger";
    pub const CONFIG: &str = "config";
    pub const RUNTIME: &str = "runtime";
    pub const VECTOR: &str = "vector";
    pub const EMBEDDING: &str = "embedding";
    pub const LLM: &str = "llm";
}

/// Service operation constants for WAFER.
pub struct ServiceOp;

impl ServiceOp {
    pub const DATABASE_GET: &str = "database.get";
    pub const DATABASE_LIST: &str = "database.list";
    pub const DATABASE_CREATE: &str = "database.create";
    pub const DATABASE_UPDATE: &str = "database.update";
    pub const DATABASE_DELETE: &str = "database.delete";
    pub const DATABASE_COUNT: &str = "database.count";
    pub const DATABASE_QUERY_RAW: &str = "database.query_raw";
    pub const DATABASE_EXEC_RAW: &str = "database.exec_raw";
    pub const DATABASE_SUM: &str = "database.sum";
    pub const DATABASE_DELETE_WHERE: &str = "database.delete_where";
    pub const DATABASE_DELETE_WHERE_COUNT: &str = "database.delete_where_count";
    pub const DATABASE_TAKE_WHERE: &str = "database.take_where";
    pub const DATABASE_UPDATE_WHERE: &str = "database.update_where";
    pub const STORAGE_PUT: &str = "storage.put";
    pub const STORAGE_GET: &str = "storage.get";
    pub const STORAGE_DELETE: &str = "storage.delete";
    pub const STORAGE_LIST: &str = "storage.list";
    pub const STORAGE_CREATE_FOLDER: &str = "storage.create_folder";
    pub const STORAGE_DELETE_FOLDER: &str = "storage.delete_folder";
    pub const STORAGE_LIST_FOLDERS: &str = "storage.list_folders";
    pub const CRYPTO_HASH: &str = "crypto.hash";
    pub const CRYPTO_COMPARE_HASH: &str = "crypto.compare_hash";
    pub const CRYPTO_SIGN: &str = "crypto.sign";
    pub const CRYPTO_VERIFY: &str = "crypto.verify";
    pub const CRYPTO_RANDOM_BYTES: &str = "crypto.random_bytes";
    pub const NETWORK_DO_REQUEST: &str = "network.do";
    pub const LOGGER_DEBUG: &str = "logger.debug";
    pub const LOGGER_INFO: &str = "logger.info";
    pub const LOGGER_WARN: &str = "logger.warn";
    pub const LOGGER_ERROR: &str = "logger.error";
    pub const CONFIG_GET: &str = "config.get";
    pub const CONFIG_SET: &str = "config.set";
    pub const VECTOR_CREATE_INDEX: &str = "vector.create_index";
    pub const VECTOR_DELETE_INDEX: &str = "vector.delete_index";
    pub const VECTOR_UPSERT: &str = "vector.upsert";
    pub const VECTOR_QUERY: &str = "vector.query";
    pub const VECTOR_DELETE: &str = "vector.delete";
    pub const VECTOR_COUNT: &str = "vector.count";
    pub const EMBEDDING_EMBED: &str = "embedding.embed";
    pub const LLM_CHAT: &str = "llm.chat";
    pub const LLM_LIST_MODELS: &str = "llm.list_models";
    pub const LLM_STATUS: &str = "llm.status";
    pub const LLM_LOAD_MODEL: &str = "llm.load_model";
    pub const LLM_UNLOAD_MODEL: &str = "llm.unload_model";
    pub const IMAGE_GENERATE: &str = "image.generate";
    pub const IMAGE_LIST_MODELS: &str = "image.list_models";
    pub const IMAGE_STATUS: &str = "image.status";
    pub const IMAGE_LOAD_MODEL: &str = "image.load_model";
    pub const IMAGE_UNLOAD_MODEL: &str = "image.unload_model";
    pub const AUTH_REQUIRE_USER: &str = "auth.require_user";
    pub const AUTH_REQUIRE_TOKEN: &str = "auth.require_token";
    pub const AUTH_REQUIRE_ROLE: &str = "auth.require_role";
    pub const AUTH_USER_PROFILE: &str = "auth.user_profile";
}
