//! Well-known interface specifications for WAFER blocks.
//!
//! Each function returns an `InterfaceSpec` describing the contract for that
//! interface — what actions it handles, expected message data shapes, and
//! response shapes. Block authors implementing an existing interface can
//! consult these specs to know exactly what to support.

use std::collections::HashMap;

use serde_json::json;

use crate::types::{ActionSpec, InterfaceSpec};

/// Return all well-known interface specs.
pub fn all() -> Vec<InterfaceSpec> {
    vec![
        middleware_v1(),
        http_handler_v1(),
        router_v1(),
        http_listener_v1(),
        database_v1(),
        storage_v1(),
        crypto_v1(),
        http_client_v1(),
        logger_v1(),
        config_v1(),
        service_v1(),
    ]
}

/// Middleware — inspects/modifies messages flowing through a pipeline.
/// Returns `Continue` to pass through, or `Respond`/`Error`/`Drop` to short-circuit.
/// Action-agnostic: handles any message kind.
pub fn middleware_v1() -> InterfaceSpec {
    InterfaceSpec {
        name: "middleware@v1".into(),
        description: "Inspects or modifies messages flowing through a pipeline. Returns Continue to pass through, or Respond/Error/Drop to short-circuit.".into(),
        actions: HashMap::new(), // action-agnostic
    }
}

/// Handler — receives HTTP-routed messages and produces responses.
pub fn http_handler_v1() -> InterfaceSpec {
    let mut actions = HashMap::new();
    actions.insert(
        "retrieve".into(),
        ActionSpec {
            description: "Handle GET requests.".into(),
            message_schema: None,
            response_schema: None,
        },
    );
    actions.insert(
        "create".into(),
        ActionSpec {
            description: "Handle POST requests.".into(),
            message_schema: None,
            response_schema: None,
        },
    );
    actions.insert(
        "update".into(),
        ActionSpec {
            description: "Handle PUT/PATCH requests.".into(),
            message_schema: None,
            response_schema: None,
        },
    );
    actions.insert(
        "delete".into(),
        ActionSpec {
            description: "Handle DELETE requests.".into(),
            message_schema: None,
            response_schema: None,
        },
    );
    InterfaceSpec {
        name: "http-handler@v1".into(),
        description: "Receives HTTP-routed messages and produces responses. Typically mounted behind a router.".into(),
        actions,
    }
}

/// Router — matches request paths and actions to handler blocks.
pub fn router_v1() -> InterfaceSpec {
    InterfaceSpec {
        name: "router@v1".into(),
        description: "Matches request paths and actions against configured routes, dispatches to handler blocks via call_block.".into(),
        actions: HashMap::new(), // delegates to handlers
    }
}

/// HTTP Listener — accepts HTTP connections and converts them to WAFER messages.
pub fn http_listener_v1() -> InterfaceSpec {
    InterfaceSpec {
        name: "http-listener@v1".into(),
        description: "Accepts HTTP connections and converts HTTP requests into WAFER messages, then converts responses back to HTTP.".into(),
        actions: HashMap::new(),
    }
}

/// Database — CRUD operations on collections plus raw SQL.
pub fn database_v1() -> InterfaceSpec {
    let filter_schema = json!({
        "type": "object",
        "properties": {
            "field": { "type": "string" },
            "operator": { "type": "string", "enum": ["eq", "neq", "gt", "gte", "lt", "lte", "like", "in", "is_null", "is_not_null"], "default": "eq" },
            "value": {}
        },
        "required": ["field"]
    });

    let sort_schema = json!({
        "type": "object",
        "properties": {
            "field": { "type": "string" },
            "desc": { "type": "boolean", "default": false }
        },
        "required": ["field"]
    });

    let mut actions = HashMap::new();

    actions.insert(
        "database.get".into(),
        ActionSpec {
            description: "Get a single record by ID.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "collection": { "type": "string" },
                    "id": { "type": "string" }
                },
                "required": ["collection", "id"]
            })),
            response_schema: Some(json!({
                "type": "object",
                "description": "The record as a JSON object."
            })),
        },
    );

    actions.insert(
        "database.list".into(),
        ActionSpec {
            description: "List records with optional filtering, sorting, and pagination.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "collection": { "type": "string" },
                    "filters": { "type": "array", "items": filter_schema },
                    "sort": { "type": "array", "items": sort_schema },
                    "limit": { "type": "integer" },
                    "offset": { "type": "integer" }
                },
                "required": ["collection"]
            })),
            response_schema: Some(json!({
                "type": "array",
                "items": { "type": "object" }
            })),
        },
    );

    actions.insert(
        "database.create".into(),
        ActionSpec {
            description: "Create a new record.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "collection": { "type": "string" },
                    "data": { "type": "object" }
                },
                "required": ["collection", "data"]
            })),
            response_schema: Some(json!({
                "type": "object",
                "description": "The created record."
            })),
        },
    );

    actions.insert(
        "database.update".into(),
        ActionSpec {
            description: "Update an existing record by ID.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "collection": { "type": "string" },
                    "id": { "type": "string" },
                    "data": { "type": "object" }
                },
                "required": ["collection", "id", "data"]
            })),
            response_schema: Some(json!({
                "type": "object",
                "description": "The updated record."
            })),
        },
    );

    actions.insert(
        "database.delete".into(),
        ActionSpec {
            description: "Delete a record by ID.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "collection": { "type": "string" },
                    "id": { "type": "string" }
                },
                "required": ["collection", "id"]
            })),
            response_schema: None,
        },
    );

    actions.insert(
        "database.count".into(),
        ActionSpec {
            description: "Count records matching optional filters.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "collection": { "type": "string" },
                    "filters": { "type": "array", "items": filter_schema }
                },
                "required": ["collection"]
            })),
            response_schema: Some(json!({
                "type": "object",
                "properties": {
                    "count": { "type": "integer" }
                }
            })),
        },
    );

    actions.insert(
        "database.query_raw".into(),
        ActionSpec {
            description: "Execute a raw SQL query and return rows.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "args": { "type": "array" }
                },
                "required": ["query"]
            })),
            response_schema: Some(json!({
                "type": "array",
                "items": { "type": "object" }
            })),
        },
    );

    actions.insert(
        "database.exec_raw".into(),
        ActionSpec {
            description: "Execute a raw SQL statement (INSERT/UPDATE/DELETE).".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "args": { "type": "array" }
                },
                "required": ["query"]
            })),
            response_schema: Some(json!({
                "type": "object",
                "properties": {
                    "rows_affected": { "type": "integer" }
                }
            })),
        },
    );

    actions.insert(
        "database.sum".into(),
        ActionSpec {
            description: "Sum a numeric field across records matching optional filters.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "collection": { "type": "string" },
                    "field": { "type": "string" },
                    "filters": { "type": "array", "items": filter_schema }
                },
                "required": ["collection", "field"]
            })),
            response_schema: Some(json!({
                "type": "object",
                "properties": {
                    "sum": { "type": "number" }
                }
            })),
        },
    );

    InterfaceSpec {
        name: "database@v1".into(),
        description: "CRUD operations on collections plus raw SQL. Actions are message kinds (e.g. database.get, database.list).".into(),
        actions,
    }
}

/// Storage — object/file storage with folders.
pub fn storage_v1() -> InterfaceSpec {
    let mut actions = HashMap::new();

    actions.insert(
        "storage.put".into(),
        ActionSpec {
            description: "Upload an object to a folder.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "folder": { "type": "string" },
                    "key": { "type": "string" },
                    "data": { "type": "string", "description": "Base64-encoded bytes" },
                    "content_type": { "type": "string", "default": "application/octet-stream" }
                },
                "required": ["folder", "key", "data"]
            })),
            response_schema: None,
        },
    );

    actions.insert(
        "storage.get".into(),
        ActionSpec {
            description: "Download an object from a folder.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "folder": { "type": "string" },
                    "key": { "type": "string" }
                },
                "required": ["folder", "key"]
            })),
            response_schema: Some(json!({
                "type": "object",
                "properties": {
                    "data": { "type": "string", "description": "Base64-encoded bytes" },
                    "info": { "type": "object" }
                }
            })),
        },
    );

    actions.insert(
        "storage.delete".into(),
        ActionSpec {
            description: "Delete an object from a folder.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "folder": { "type": "string" },
                    "key": { "type": "string" }
                },
                "required": ["folder", "key"]
            })),
            response_schema: None,
        },
    );

    actions.insert(
        "storage.list".into(),
        ActionSpec {
            description: "List objects in a folder with optional prefix filter.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "folder": { "type": "string" },
                    "prefix": { "type": "string" },
                    "limit": { "type": "integer" },
                    "offset": { "type": "integer" }
                },
                "required": ["folder"]
            })),
            response_schema: Some(json!({
                "type": "array",
                "items": { "type": "object" }
            })),
        },
    );

    actions.insert(
        "storage.create_folder".into(),
        ActionSpec {
            description: "Create a new storage folder (bucket).".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "public": { "type": "boolean", "default": false }
                },
                "required": ["name"]
            })),
            response_schema: None,
        },
    );

    actions.insert(
        "storage.delete_folder".into(),
        ActionSpec {
            description: "Delete a storage folder.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" }
                },
                "required": ["name"]
            })),
            response_schema: None,
        },
    );

    actions.insert(
        "storage.list_folders".into(),
        ActionSpec {
            description: "List all storage folders.".into(),
            message_schema: None,
            response_schema: Some(json!({
                "type": "array",
                "items": { "type": "object" }
            })),
        },
    );

    InterfaceSpec {
        name: "storage@v1".into(),
        description: "Object/file storage with folder-based organization. Actions are message kinds (e.g. storage.put, storage.get).".into(),
        actions,
    }
}

/// Crypto — hashing, JWT signing/verification, random bytes.
pub fn crypto_v1() -> InterfaceSpec {
    let mut actions = HashMap::new();

    actions.insert(
        "crypto.hash".into(),
        ActionSpec {
            description: "Hash a password (bcrypt/argon2).".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "password": { "type": "string" }
                },
                "required": ["password"]
            })),
            response_schema: Some(json!({
                "type": "object",
                "properties": {
                    "hash": { "type": "string" }
                }
            })),
        },
    );

    actions.insert(
        "crypto.compare_hash".into(),
        ActionSpec {
            description: "Compare a password against a hash.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "password": { "type": "string" },
                    "hash": { "type": "string" }
                },
                "required": ["password", "hash"]
            })),
            response_schema: Some(json!({
                "type": "object",
                "properties": {
                    "match": { "type": "boolean" }
                }
            })),
        },
    );

    actions.insert(
        "crypto.sign".into(),
        ActionSpec {
            description: "Sign a JWT with the given claims.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "claims": { "type": "object" },
                    "expiry_secs": { "type": "integer", "default": 3600 }
                },
                "required": ["claims"]
            })),
            response_schema: Some(json!({
                "type": "object",
                "properties": {
                    "token": { "type": "string" }
                }
            })),
        },
    );

    actions.insert(
        "crypto.verify".into(),
        ActionSpec {
            description: "Verify a JWT and return its claims.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "token": { "type": "string" }
                },
                "required": ["token"]
            })),
            response_schema: Some(json!({
                "type": "object",
                "properties": {
                    "claims": { "type": "object" }
                }
            })),
        },
    );

    actions.insert(
        "crypto.random_bytes".into(),
        ActionSpec {
            description: "Generate cryptographically secure random bytes.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "n": { "type": "integer", "default": 32, "maximum": 1048576 }
                }
            })),
            response_schema: Some(json!({
                "type": "object",
                "properties": {
                    "bytes": { "type": "string", "description": "Base64-encoded random bytes" }
                }
            })),
        },
    );

    InterfaceSpec {
        name: "crypto@v1".into(),
        description: "Password hashing, JWT signing/verification, and random byte generation."
            .into(),
        actions,
    }
}

/// Network — outbound HTTP requests.
pub fn http_client_v1() -> InterfaceSpec {
    let mut actions = HashMap::new();

    actions.insert(
        "network.do".into(),
        ActionSpec {
            description: "Make an outbound HTTP request.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "method": { "type": "string" },
                    "url": { "type": "string" },
                    "headers": { "type": "object", "additionalProperties": { "type": "string" } },
                    "body": { "type": "string", "description": "Base64-encoded request body" }
                },
                "required": ["method", "url"]
            })),
            response_schema: Some(json!({
                "type": "object",
                "properties": {
                    "status_code": { "type": "integer" },
                    "headers": { "type": "object" },
                    "body": { "type": "string", "description": "Base64-encoded response body" }
                }
            })),
        },
    );

    InterfaceSpec {
        name: "http-client@v1".into(),
        description:
            "Outbound HTTP requests. Provides a single network.do action for making HTTP calls."
                .into(),
        actions,
    }
}

/// Logger — structured logging at different levels.
pub fn logger_v1() -> InterfaceSpec {
    let log_msg_schema = json!({
        "type": "object",
        "properties": {
            "message": { "type": "string" },
            "fields": { "type": "object" }
        }
    });

    let mut actions = HashMap::new();
    for level in &["debug", "info", "warn", "error"] {
        actions.insert(
            format!("logger.{level}"),
            ActionSpec {
                description: format!("Log a message at {level} level."),
                message_schema: Some(log_msg_schema.clone()),
                response_schema: None,
            },
        );
    }

    InterfaceSpec {
        name: "logger@v1".into(),
        description: "Structured logging at debug/info/warn/error levels.".into(),
        actions,
    }
}

/// Config — runtime configuration access.
pub fn config_v1() -> InterfaceSpec {
    InterfaceSpec {
        name: "config@v1".into(),
        description: "Provides runtime configuration values to blocks.".into(),
        actions: HashMap::new(),
    }
}

/// Service — general-purpose service block (called on demand via call_block).
pub fn service_v1() -> InterfaceSpec {
    InterfaceSpec {
        name: "service@v1".into(),
        description: "General-purpose service block, called on demand via call_block. Message format is block-specific.".into(),
        actions: HashMap::new(),
    }
}
