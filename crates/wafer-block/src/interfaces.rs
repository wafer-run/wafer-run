//! Well-known interface specifications for WAFER blocks.
//!
//! Each function returns an `InterfaceSpec` describing the contract for that
//! interface — what actions it handles, expected message data shapes, and
//! response shapes. Block authors implementing an existing interface can
//! consult these specs to know exactly what to support.
//!
//! Service-backed interfaces derive their action maps from the canonical
//! [`ServiceOp`] op-family slices (e.g. [`ServiceOp::DATABASE_OPS`]), so the
//! catalog the dispatcher validates against cannot drift from the op list the
//! service handlers implement. Adding an op to a family slice without an
//! `ActionSpec` arm panics in the `*_action_spec` fn — caught immediately by
//! the drift tests in this module (and by any `Wafer` construction).

use std::collections::HashMap;

use serde_json::json;

use crate::{
    common::ServiceOp,
    types::{ActionSpec, InterfaceSpec},
};

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

/// Build an interface's action map from its canonical op-family slice.
///
/// `spec_for` must return an [`ActionSpec`] for every op in `ops` and panic
/// otherwise; the drift tests in this module call every interface builder, so
/// an op added to a [`ServiceOp`] family without a matching spec fails the
/// test suite instead of drifting silently.
fn actions_from_ops(ops: &[&str], spec_for: fn(&str) -> ActionSpec) -> HashMap<String, ActionSpec> {
    ops.iter()
        .map(|&op| (op.to_string(), spec_for(op)))
        .collect()
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

/// JSON Schema for a single filter predicate, shared by the filtered
/// database actions.
fn filter_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "field": { "type": "string" },
            "operator": { "type": "string", "enum": ["eq", "neq", "gt", "gte", "lt", "lte", "like", "in", "is_null", "is_not_null"], "default": "eq" },
            "value": {}
        },
        "required": ["field"]
    })
}

/// JSON Schema for a single sort directive, shared by the database list action.
fn sort_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "field": { "type": "string" },
            "desc": { "type": "boolean", "default": false }
        },
        "required": ["field"]
    })
}

/// Database — CRUD operations on collections plus raw SQL.
pub fn database_v1() -> InterfaceSpec {
    InterfaceSpec {
        name: "database@v1".into(),
        description: "CRUD operations on collections plus raw SQL. Actions are message kinds (e.g. database.get, database.list).".into(),
        actions: actions_from_ops(ServiceOp::DATABASE_OPS, database_action_spec),
    }
}

/// `ActionSpec` for a single `database.*` op.
fn database_action_spec(op: &str) -> ActionSpec {
    match op {
        ServiceOp::DATABASE_GET => ActionSpec {
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
        ServiceOp::DATABASE_LIST => ActionSpec {
            description: "List records with optional filtering, sorting, and pagination.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "collection": { "type": "string" },
                    "filters": { "type": "array", "items": filter_schema() },
                    "sort": { "type": "array", "items": sort_schema() },
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
        ServiceOp::DATABASE_CREATE => ActionSpec {
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
        ServiceOp::DATABASE_UPDATE => ActionSpec {
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
        ServiceOp::DATABASE_UPDATE_WHERE => ActionSpec {
            description:
                "Update fields on all records in a collection that match a set of filters.".into(),
            message_schema: None,
            response_schema: None,
        },
        ServiceOp::DATABASE_DELETE => ActionSpec {
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
        ServiceOp::DATABASE_DELETE_WHERE => ActionSpec {
            description: "Delete all records in a collection that match a set of filters.".into(),
            message_schema: None,
            response_schema: None,
        },
        ServiceOp::DATABASE_DELETE_WHERE_COUNT => ActionSpec {
            description: "Delete all records matching a set of filters and return the number of records deleted.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "collection": { "type": "string" },
                    "filters": { "type": "array", "items": filter_schema() }
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
        ServiceOp::DATABASE_TAKE_WHERE => ActionSpec {
            description: "Atomically delete and return all records matching a set of filters."
                .into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "collection": { "type": "string" },
                    "filters": { "type": "array", "items": filter_schema() }
                },
                "required": ["collection"]
            })),
            response_schema: Some(json!({
                "type": "object",
                "properties": {
                    "records": { "type": "array", "items": { "type": "object" } }
                }
            })),
        },
        ServiceOp::DATABASE_COUNT => ActionSpec {
            description: "Count records matching optional filters.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "collection": { "type": "string" },
                    "filters": { "type": "array", "items": filter_schema() }
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
        ServiceOp::DATABASE_SUM => ActionSpec {
            description: "Sum a numeric field across records matching optional filters.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "collection": { "type": "string" },
                    "field": { "type": "string" },
                    "filters": { "type": "array", "items": filter_schema() }
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
        ServiceOp::DATABASE_INCREMENT_FIELD_WHERE => ActionSpec {
            description: "Atomically increment a numeric column on all records matching a set of filters.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "collection": { "type": "string" },
                    "col": { "type": "string" },
                    "delta": { "type": "integer", "description": "Signed delta to add (negative = decrement)" },
                    "filters": { "type": "array", "items": filter_schema() }
                },
                "required": ["collection", "col", "delta"]
            })),
            response_schema: Some(json!({
                "type": "object",
                "properties": {
                    "rows_affected": { "type": "integer" }
                }
            })),
        },
        ServiceOp::DATABASE_QUERY => ActionSpec {
            description: "Execute a typed SQL query, WRAP-authorized against its target collection, and return rows.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "sql": { "type": "string" },
                    "args": { "type": "array" },
                    "collection": { "type": "string", "description": "Collection (table) the statement targets. WRAP-authorized." }
                },
                "required": ["sql", "collection"]
            })),
            response_schema: Some(json!({
                "type": "object",
                "properties": {
                    "rows": { "type": "array", "items": { "type": "object" } }
                }
            })),
        },
        ServiceOp::DATABASE_QUERY_RAW => ActionSpec {
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
        ServiceOp::DATABASE_EXECUTE => ActionSpec {
            description:
                "Execute a typed SQL mutation, WRAP-authorized against its target collection."
                    .into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "sql": { "type": "string" },
                    "args": { "type": "array" },
                    "collection": { "type": "string", "description": "Collection (table) the statement targets. WRAP-authorized." }
                },
                "required": ["sql", "collection"]
            })),
            response_schema: Some(json!({
                "type": "object",
                "properties": {
                    "rows_affected": { "type": "integer" }
                }
            })),
        },
        ServiceOp::DATABASE_EXEC_RAW => ActionSpec {
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
        ServiceOp::DATABASE_DDL => ActionSpec {
            description: "Execute a raw DDL statement (cap-gated; host-authoritative op distinct from database.exec_raw).".into(),
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
        other => panic!(
            "BUG: no ActionSpec for database op '{other}' — update database_action_spec alongside ServiceOp::DATABASE_OPS"
        ),
    }
}

/// Storage — object/file storage with folders.
pub fn storage_v1() -> InterfaceSpec {
    InterfaceSpec {
        name: "storage@v1".into(),
        description: "Object/file storage with folder-based organization. Actions are message kinds (e.g. storage.put, storage.get).".into(),
        actions: actions_from_ops(ServiceOp::STORAGE_OPS, storage_action_spec),
    }
}

/// `ActionSpec` for a single `storage.*` op.
fn storage_action_spec(op: &str) -> ActionSpec {
    match op {
        ServiceOp::STORAGE_PUT => ActionSpec {
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
        ServiceOp::STORAGE_GET => ActionSpec {
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
        ServiceOp::STORAGE_DELETE => ActionSpec {
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
        ServiceOp::STORAGE_LIST => ActionSpec {
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
        ServiceOp::STORAGE_CREATE_FOLDER => ActionSpec {
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
        ServiceOp::STORAGE_DELETE_FOLDER => ActionSpec {
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
        ServiceOp::STORAGE_LIST_FOLDERS => ActionSpec {
            description: "List all storage folders.".into(),
            message_schema: None,
            response_schema: Some(json!({
                "type": "array",
                "items": { "type": "object" }
            })),
        },
        other => panic!(
            "BUG: no ActionSpec for storage op '{other}' — update storage_action_spec alongside ServiceOp::STORAGE_OPS"
        ),
    }
}

/// Crypto — hashing, JWT signing/verification, random bytes.
pub fn crypto_v1() -> InterfaceSpec {
    InterfaceSpec {
        name: "crypto@v1".into(),
        description: "Password hashing, JWT signing/verification, and random byte generation."
            .into(),
        actions: actions_from_ops(ServiceOp::CRYPTO_OPS, crypto_action_spec),
    }
}

/// `ActionSpec` for a single `crypto.*` op.
fn crypto_action_spec(op: &str) -> ActionSpec {
    match op {
        ServiceOp::CRYPTO_HASH => ActionSpec {
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
        ServiceOp::CRYPTO_COMPARE_HASH => ActionSpec {
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
        ServiceOp::CRYPTO_SIGN => ActionSpec {
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
        ServiceOp::CRYPTO_VERIFY => ActionSpec {
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
        ServiceOp::CRYPTO_RANDOM_BYTES => ActionSpec {
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
        other => panic!(
            "BUG: no ActionSpec for crypto op '{other}' — update crypto_action_spec alongside ServiceOp::CRYPTO_OPS"
        ),
    }
}

/// Network — outbound HTTP requests.
pub fn http_client_v1() -> InterfaceSpec {
    InterfaceSpec {
        name: "http-client@v1".into(),
        description:
            "Outbound HTTP requests. Provides a single network.do action for making HTTP calls."
                .into(),
        actions: actions_from_ops(ServiceOp::NETWORK_OPS, network_action_spec),
    }
}

/// `ActionSpec` for a single `network.*` op.
fn network_action_spec(op: &str) -> ActionSpec {
    match op {
        ServiceOp::NETWORK_DO_REQUEST => ActionSpec {
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
        other => panic!(
            "BUG: no ActionSpec for network op '{other}' — update network_action_spec alongside ServiceOp::NETWORK_OPS"
        ),
    }
}

/// Logger — structured logging at different levels.
pub fn logger_v1() -> InterfaceSpec {
    InterfaceSpec {
        name: "logger@v1".into(),
        description: "Structured logging at debug/info/warn/error levels.".into(),
        actions: actions_from_ops(ServiceOp::LOGGER_OPS, logger_action_spec),
    }
}

/// `ActionSpec` for a single `logger.*` op.
fn logger_action_spec(op: &str) -> ActionSpec {
    let level = match op {
        ServiceOp::LOGGER_DEBUG => "debug",
        ServiceOp::LOGGER_INFO => "info",
        ServiceOp::LOGGER_WARN => "warn",
        ServiceOp::LOGGER_ERROR => "error",
        other => panic!(
            "BUG: no ActionSpec for logger op '{other}' — update logger_action_spec alongside ServiceOp::LOGGER_OPS"
        ),
    };
    ActionSpec {
        description: format!("Log a message at {level} level."),
        message_schema: Some(json!({
            "type": "object",
            "properties": {
                "message": { "type": "string" },
                "fields": { "type": "object" }
            }
        })),
        response_schema: None,
    }
}

/// Config — runtime configuration access.
pub fn config_v1() -> InterfaceSpec {
    InterfaceSpec {
        name: "config@v1".into(),
        description: "Provides runtime configuration values to blocks. Actions are message kinds (config.get, config.set).".into(),
        actions: actions_from_ops(ServiceOp::CONFIG_OPS, config_action_spec),
    }
}

/// `ActionSpec` for a single `config.*` op.
fn config_action_spec(op: &str) -> ActionSpec {
    match op {
        ServiceOp::CONFIG_GET => ActionSpec {
            description: "Read a config value by key.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "key": { "type": "string" }
                },
                "required": ["key"]
            })),
            response_schema: Some(json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string", "description": "Resolved value, or \"\" when the key is unset" }
                }
            })),
        },
        ServiceOp::CONFIG_SET => ActionSpec {
            description: "Write a config value.".into(),
            message_schema: Some(json!({
                "type": "object",
                "properties": {
                    "key": { "type": "string" },
                    "value": { "type": "string" }
                },
                "required": ["key", "value"]
            })),
            response_schema: None,
        },
        other => panic!(
            "BUG: no ActionSpec for config op '{other}' — update config_action_spec alongside ServiceOp::CONFIG_OPS"
        ),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every interface whose action catalog is derived from a [`ServiceOp`]
    /// op-family slice, paired with that slice.
    fn catalogued() -> Vec<(InterfaceSpec, &'static [&'static str])> {
        vec![
            (database_v1(), ServiceOp::DATABASE_OPS),
            (storage_v1(), ServiceOp::STORAGE_OPS),
            (crypto_v1(), ServiceOp::CRYPTO_OPS),
            (http_client_v1(), ServiceOp::NETWORK_OPS),
            (logger_v1(), ServiceOp::LOGGER_OPS),
            (config_v1(), ServiceOp::CONFIG_OPS),
        ]
    }

    /// Drift guard: every op in a `ServiceOp` family slice has an entry in the
    /// matching interface catalog, and the catalog contains nothing else.
    /// (Building the catalog from the slice also makes a missing `ActionSpec`
    /// arm panic right here.)
    #[test]
    fn action_catalogs_match_service_op_families_exactly() {
        for (spec, ops) in catalogued() {
            for op in ops {
                assert!(
                    spec.actions.contains_key(*op),
                    "interface '{}' is missing an ActionSpec for op '{op}'",
                    spec.name
                );
            }
            assert_eq!(
                spec.actions.len(),
                ops.len(),
                "interface '{}' catalog and its ServiceOp family differ in size \
                 (duplicate slice entry or hand-added action?)",
                spec.name
            );
        }
    }

    /// Every op in a family slice carries that family's `service.` prefix —
    /// catches a constant pasted into the wrong family slice.
    #[test]
    fn op_family_slices_have_consistent_prefixes() {
        for (prefix, ops) in [
            ("database.", ServiceOp::DATABASE_OPS),
            ("storage.", ServiceOp::STORAGE_OPS),
            ("crypto.", ServiceOp::CRYPTO_OPS),
            ("network.", ServiceOp::NETWORK_OPS),
            ("logger.", ServiceOp::LOGGER_OPS),
            ("config.", ServiceOp::CONFIG_OPS),
        ] {
            for op in ops {
                assert!(
                    op.starts_with(prefix),
                    "op '{op}' does not belong in the '{prefix}*' family slice"
                );
            }
        }
    }

    /// Regression pin for the 2026-06-10 audit finding: these five ops were
    /// implemented by the database handler but missing from the hand-written
    /// catalog, so dispatch validation rejected them with INVALID_ARGUMENT.
    #[test]
    fn database_catalog_includes_previously_missing_ops() {
        let spec = database_v1();
        for op in [
            "database.query",
            "database.execute",
            "database.take_where",
            "database.delete_where_count",
            "database.increment_field_where",
        ] {
            assert!(
                spec.actions.contains_key(op),
                "database@v1 catalog regressed: op '{op}' is missing again"
            );
        }
    }

    /// `all()` registers every catalogued interface (so the runtime's
    /// dispatch validation actually sees the derived action maps).
    #[test]
    fn all_includes_every_catalogued_interface() {
        let names: Vec<String> = all().into_iter().map(|s| s.name).collect();
        for (spec, _) in catalogued() {
            assert!(
                names.contains(&spec.name),
                "interfaces::all() does not register '{}'",
                spec.name
            );
        }
    }
}
