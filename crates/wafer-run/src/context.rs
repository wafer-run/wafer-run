use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::HashMap;
use std::time::Duration;

use crate::common::ErrorCode;
use crate::services::database::{DatabaseError, Filter, FilterOp, ListOptions, SortField};
use crate::services::storage::{ListOptions as StorageListOptions, StorageError};
use crate::services::Services;
use crate::types::*;
use crate::wasm::capabilities::BlockCapabilities;

/// CapabilityInfo describes a runtime capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityInfo {
    pub kind: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
}

/// Context provides runtime capabilities to blocks via message passing.
pub trait Context: Send + Sync {
    /// Send a message to a runtime capability (log, config, dispatch, etc.)
    fn send(&self, msg: &Message) -> Result_;

    /// Capabilities returns available runtime capabilities.
    fn capabilities(&self) -> Vec<CapabilityInfo>;

    /// Check if the context has been cancelled.
    fn is_cancelled(&self) -> bool;

    /// Service returns a named service registered on the runtime, or None.
    fn service(&self, name: &str) -> Option<&dyn Any>;

    /// Services returns the typed platform services.
    fn services(&self) -> Option<&Services>;

    /// Get a config value from the block's node config.
    fn config_get(&self, key: &str) -> Option<&str>;
}

/// RuntimeContext implements Context for blocks.
pub struct RuntimeContext {
    pub chain_id: String,
    pub node_id: String,
    pub config: HashMap<String, String>,
    pub cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub deadline: Option<std::time::Instant>,
    pub named_services:
        std::sync::Arc<HashMap<String, Box<dyn Any + Send + Sync>>>,
    pub platform_services: Option<std::sync::Arc<Services>>,
    /// Capability restrictions for this block. None = unrestricted (native blocks).
    pub capabilities: Option<BlockCapabilities>,
}

// --- Wire format types for deserializing guest messages ---

#[derive(Deserialize)]
struct WireListOptions {
    #[serde(default)]
    filters: Vec<WireFilter>,
    #[serde(default)]
    sort: Vec<WireSortField>,
    #[serde(default)]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

#[derive(Deserialize)]
struct WireFilter {
    field: String,
    operator: String,
    value: serde_json::Value,
}

#[derive(Deserialize)]
struct WireSortField {
    field: String,
    #[serde(default)]
    desc: bool,
}

impl WireListOptions {
    fn into_list_options(self) -> ListOptions {
        ListOptions {
            filters: self
                .filters
                .into_iter()
                .map(|f| Filter {
                    field: f.field,
                    operator: parse_filter_op(&f.operator),
                    value: f.value,
                })
                .collect(),
            sort: self
                .sort
                .into_iter()
                .map(|s| SortField {
                    field: s.field,
                    desc: s.desc,
                })
                .collect(),
            limit: self.limit,
            offset: self.offset,
        }
    }
}

fn parse_filter_op(op: &str) -> FilterOp {
    match op {
        "eq" => FilterOp::Equal,
        "neq" => FilterOp::NotEqual,
        "gt" => FilterOp::GreaterThan,
        "gte" => FilterOp::GreaterEqual,
        "lt" => FilterOp::LessThan,
        "lte" => FilterOp::LessEqual,
        "like" => FilterOp::Like,
        "in" => FilterOp::In,
        _ => FilterOp::Equal,
    }
}

// --- Result helpers ---

fn ok_respond(data: Vec<u8>) -> Result_ {
    Result_ {
        action: Action::Respond,
        response: Some(Response {
            data,
            meta: HashMap::new(),
        }),
        error: None,
        message: None,
    }
}

fn ok_empty() -> Result_ {
    Result_ {
        action: Action::Respond,
        response: Some(Response {
            data: Vec::new(),
            meta: HashMap::new(),
        }),
        error: None,
        message: None,
    }
}

fn ok_continue() -> Result_ {
    Result_ {
        action: Action::Continue,
        response: None,
        error: None,
        message: None,
    }
}

fn err_result(code: impl Into<String>, message: impl Into<String>) -> Result_ {
    Result_ {
        action: Action::Error,
        response: None,
        error: Some(WaferError::new(code, message)),
        message: None,
    }
}

fn json_respond<T: Serialize>(v: &T) -> Result_ {
    match serde_json::to_vec(v) {
        Ok(data) => ok_respond(data),
        Err(e) => err_result(ErrorCode::INTERNAL, format!("failed to serialize response: {e}")),
    }
}

/// Helper to require a service or return an UNAVAILABLE error.
fn require_service<'a, T>(service: &'a Option<T>, name: &str) -> std::result::Result<&'a T, Result_> {
    service
        .as_ref()
        .ok_or_else(|| err_result(ErrorCode::UNAVAILABLE, format!("{name} service not configured")))
}

fn db_err(e: DatabaseError) -> Result_ {
    match e {
        DatabaseError::NotFound => err_result(ErrorCode::NOT_FOUND, "record not found"),
        DatabaseError::Internal(msg) => err_result(ErrorCode::INTERNAL, msg),
        DatabaseError::Other(err) => err_result(ErrorCode::INTERNAL, err.to_string()),
    }
}

fn storage_err(e: StorageError) -> Result_ {
    match e {
        StorageError::NotFound => err_result(ErrorCode::NOT_FOUND, "object not found"),
        StorageError::Internal(msg) => err_result(ErrorCode::INTERNAL, msg),
        StorageError::Other(err) => err_result(ErrorCode::INTERNAL, err.to_string()),
    }
}

/// Simple base64 encoder to avoid adding a dependency.
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

impl Context for RuntimeContext {
    fn send(&self, msg: &Message) -> Result_ {
        let kind = msg.kind.as_str();

        // Route svc.* messages to platform services
        if let Some(svc_kind) = kind.strip_prefix("svc.") {
            return self.dispatch_service(svc_kind, msg);
        }

        match kind {
            "log" => {
                let level = msg.get_meta("level");
                let data = String::from_utf8_lossy(&msg.data);
                tracing::info!(chain_id = %self.chain_id, node_id = %self.node_id, level = %level, "{}", data);
                ok_continue()
            }
            "config.get" => {
                let key = msg.get_meta("key");
                match self.config.get(key) {
                    Some(val) => ok_respond(val.as_bytes().to_vec()),
                    None => err_result(ErrorCode::NOT_FOUND, format!("config key not found: {key}")),
                }
            }
            _ => err_result(ErrorCode::UNAVAILABLE, format!("unknown capability: {kind}")),
        }
    }

    fn capabilities(&self) -> Vec<CapabilityInfo> {
        vec![
            CapabilityInfo {
                kind: "log".to_string(),
                summary: "Write log message".to_string(),
                input: None,
                output: None,
            },
            CapabilityInfo {
                kind: "config.get".to_string(),
                summary: "Get configuration value".to_string(),
                input: None,
                output: None,
            },
        ]
    }

    fn is_cancelled(&self) -> bool {
        if self.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            return true;
        }
        if let Some(deadline) = self.deadline {
            if std::time::Instant::now() >= deadline {
                self.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                return true;
            }
        }
        false
    }

    fn service(&self, name: &str) -> Option<&dyn Any> {
        self.named_services
            .get(name)
            .map(|s| s.as_ref() as &dyn Any)
    }

    fn services(&self) -> Option<&Services> {
        self.platform_services.as_deref()
    }

    fn config_get(&self, key: &str) -> Option<&str> {
        self.config.get(key).map(|s| s.as_str())
    }
}

impl RuntimeContext {
    /// Check capability restrictions before dispatching. Returns Some(err) if blocked.
    fn check_capability(&self, svc_kind: &str, msg: &Message) -> Option<Result_> {
        let caps = match &self.capabilities {
            Some(c) => c,
            None => return None, // No restrictions (native block)
        };

        // Logger is always allowed
        if svc_kind.starts_with("logger.") {
            return None;
        }

        // Database operations
        if svc_kind.starts_with("database.") {
            if svc_kind == "database.query_raw" || svc_kind == "database.exec_raw" {
                if !caps.raw_sql {
                    return Some(err_result(
                        ErrorCode::PERMISSION_DENIED,
                        format!("block not allowed to use {svc_kind}: raw_sql not permitted"),
                    ));
                }
            } else {
                let collection = msg.get_meta("collection");
                if !collection.is_empty() && !caps.allows_collection(collection) {
                    return Some(err_result(
                        ErrorCode::PERMISSION_DENIED,
                        format!(
                            "block not allowed to access collection {:?}",
                            collection
                        ),
                    ));
                }
            }
            return None;
        }

        // Storage operations
        if svc_kind.starts_with("storage.") {
            let bucket = msg.get_meta("bucket");
            if !bucket.is_empty() && !caps.allows_storage_folder(bucket) {
                return Some(err_result(
                    ErrorCode::PERMISSION_DENIED,
                    format!("block not allowed to access storage folder {:?}", bucket),
                ));
            }
            return None;
        }

        // Crypto operations
        if svc_kind.starts_with("crypto.") {
            if !caps.crypto {
                return Some(err_result(
                    ErrorCode::PERMISSION_DENIED,
                    "block not allowed to use crypto service",
                ));
            }
            return None;
        }

        // Network operations
        if svc_kind == "network.do" {
            if !caps.network {
                return Some(err_result(
                    ErrorCode::PERMISSION_DENIED,
                    "block not allowed to use network service",
                ));
            }
            // Check URL allowlist
            #[derive(serde::Deserialize)]
            struct UrlPeek {
                #[serde(default)]
                url: String,
            }
            if let Ok(peek) = serde_json::from_slice::<UrlPeek>(&msg.data) {
                if !caps.allows_network_url(&peek.url) {
                    return Some(err_result(
                        ErrorCode::PERMISSION_DENIED,
                        format!("block not allowed to access URL {:?}", peek.url),
                    ));
                }
            }
            return None;
        }

        // Config operations
        if svc_kind.starts_with("config.") {
            if !caps.config {
                return Some(err_result(
                    ErrorCode::PERMISSION_DENIED,
                    "block not allowed to use config service",
                ));
            }
            let key = msg.get_meta("key");
            if !key.is_empty() && !caps.allows_config_key(key) {
                return Some(err_result(
                    ErrorCode::PERMISSION_DENIED,
                    format!("block not allowed to access config key {:?}", key),
                ));
            }
            return None;
        }

        None
    }

    /// Dispatch a svc.* message to the appropriate platform service.
    fn dispatch_service(&self, svc_kind: &str, msg: &Message) -> Result_ {
        // Capability enforcement
        if let Some(denied) = self.check_capability(svc_kind, msg) {
            return denied;
        }

        let services = match &self.platform_services {
            Some(s) => s,
            None => return err_result(ErrorCode::UNAVAILABLE, "platform services not configured"),
        };

        match svc_kind {
            // --- Database ---
            "database.get" => {
                let db = match require_service(&services.database, "database") {
                    Ok(db) => db,
                    Err(e) => return e,
                };
                let collection = msg.get_meta("collection");
                let id = msg.get_meta("id");
                match db.get(collection, id) {
                    Ok(record) => json_respond(&record),
                    Err(e) => db_err(e),
                }
            }

            "database.list" => {
                let db = match require_service(&services.database, "database") {
                    Ok(db) => db,
                    Err(e) => return e,
                };
                let collection = msg.get_meta("collection");
                let opts = if msg.data.is_empty() {
                    ListOptions::default()
                } else {
                    match serde_json::from_slice::<WireListOptions>(&msg.data) {
                        Ok(wire) => wire.into_list_options(),
                        Err(e) => {
                            return err_result(
                                ErrorCode::INVALID_ARGUMENT,
                                format!("invalid list options: {e}"),
                            )
                        }
                    }
                };
                match db.list(collection, &opts) {
                    Ok(record_list) => json_respond(&record_list),
                    Err(e) => db_err(e),
                }
            }

            "database.create" => {
                let db = match require_service(&services.database, "database") {
                    Ok(db) => db,
                    Err(e) => return e,
                };
                let collection = msg.get_meta("collection");
                let data: HashMap<String, serde_json::Value> = match serde_json::from_slice(&msg.data) {
                    Ok(d) => d,
                    Err(e) => {
                        return err_result(
                            ErrorCode::INVALID_ARGUMENT,
                            format!("invalid record data: {e}"),
                        )
                    }
                };
                match db.create(collection, data) {
                    Ok(record) => json_respond(&record),
                    Err(e) => db_err(e),
                }
            }

            "database.update" => {
                let db = match require_service(&services.database, "database") {
                    Ok(db) => db,
                    Err(e) => return e,
                };
                let collection = msg.get_meta("collection");
                let id = msg.get_meta("id");
                let data: HashMap<String, serde_json::Value> = match serde_json::from_slice(&msg.data) {
                    Ok(d) => d,
                    Err(e) => {
                        return err_result(
                            ErrorCode::INVALID_ARGUMENT,
                            format!("invalid record data: {e}"),
                        )
                    }
                };
                match db.update(collection, id, data) {
                    Ok(record) => json_respond(&record),
                    Err(e) => db_err(e),
                }
            }

            "database.delete" => {
                let db = match require_service(&services.database, "database") {
                    Ok(db) => db,
                    Err(e) => return e,
                };
                let collection = msg.get_meta("collection");
                let id = msg.get_meta("id");
                match db.delete(collection, id) {
                    Ok(()) => ok_empty(),
                    Err(e) => db_err(e),
                }
            }

            "database.count" => {
                let db = match require_service(&services.database, "database") {
                    Ok(db) => db,
                    Err(e) => return e,
                };
                let collection = msg.get_meta("collection");
                let filters: Vec<Filter> = if msg.data.is_empty() {
                    Vec::new()
                } else {
                    match serde_json::from_slice::<Vec<WireFilter>>(&msg.data) {
                        Ok(wf) => wf
                            .into_iter()
                            .map(|f| Filter {
                                field: f.field,
                                operator: parse_filter_op(&f.operator),
                                value: f.value,
                            })
                            .collect(),
                        Err(e) => {
                            return err_result(
                                ErrorCode::INVALID_ARGUMENT,
                                format!("invalid filters: {e}"),
                            )
                        }
                    }
                };
                match db.count(collection, &filters) {
                    Ok(count) => json_respond(&count),
                    Err(e) => db_err(e),
                }
            }

            "database.query_raw" => {
                let db = match require_service(&services.database, "database") {
                    Ok(db) => db,
                    Err(e) => return e,
                };
                #[derive(Deserialize)]
                struct RawQuery {
                    query: String,
                    #[serde(default)]
                    args: Vec<serde_json::Value>,
                }
                let rq: RawQuery = match serde_json::from_slice(&msg.data) {
                    Ok(q) => q,
                    Err(e) => {
                        return err_result(ErrorCode::INVALID_ARGUMENT, format!("invalid query: {e}"))
                    }
                };
                match db.query_raw(&rq.query, &rq.args) {
                    Ok(records) => json_respond(&records),
                    Err(e) => db_err(e),
                }
            }

            "database.exec_raw" => {
                let db = match require_service(&services.database, "database") {
                    Ok(db) => db,
                    Err(e) => return e,
                };
                #[derive(Deserialize)]
                struct RawExec {
                    query: String,
                    #[serde(default)]
                    args: Vec<serde_json::Value>,
                }
                let rq: RawExec = match serde_json::from_slice(&msg.data) {
                    Ok(q) => q,
                    Err(e) => {
                        return err_result(ErrorCode::INVALID_ARGUMENT, format!("invalid query: {e}"))
                    }
                };
                match db.exec_raw(&rq.query, &rq.args) {
                    Ok(affected) => json_respond(&affected),
                    Err(e) => db_err(e),
                }
            }

            // --- Storage ---
            "storage.put" => {
                let storage = match require_service(&services.storage, "storage") {
                    Ok(s) => s,
                    Err(e) => return e,
                };
                let bucket = msg.get_meta("bucket");
                let key = msg.get_meta("key");
                let content_type = {
                    let ct = msg.get_meta("content_type");
                    if ct.is_empty() {
                        "application/octet-stream"
                    } else {
                        ct
                    }
                };
                match storage.put(bucket, key, &msg.data, content_type) {
                    Ok(()) => ok_empty(),
                    Err(e) => storage_err(e),
                }
            }

            "storage.get" => {
                let storage = match require_service(&services.storage, "storage") {
                    Ok(s) => s,
                    Err(e) => return e,
                };
                let bucket = msg.get_meta("bucket");
                let key = msg.get_meta("key");
                match storage.get(bucket, key) {
                    Ok((data, _info)) => ok_respond(data),
                    Err(e) => storage_err(e),
                }
            }

            "storage.delete" => {
                let storage = match require_service(&services.storage, "storage") {
                    Ok(s) => s,
                    Err(e) => return e,
                };
                let bucket = msg.get_meta("bucket");
                let key = msg.get_meta("key");
                match storage.delete(bucket, key) {
                    Ok(()) => ok_empty(),
                    Err(e) => storage_err(e),
                }
            }

            "storage.list" => {
                let storage = match require_service(&services.storage, "storage") {
                    Ok(s) => s,
                    Err(e) => return e,
                };
                let bucket = msg.get_meta("bucket");
                let opts = StorageListOptions {
                    prefix: msg.get_meta("prefix").to_string(),
                    limit: msg
                        .get_meta("limit")
                        .parse::<i64>()
                        .unwrap_or(1000),
                    offset: msg.get_meta("offset").parse::<i64>().unwrap_or(0),
                };
                match storage.list(bucket, &opts) {
                    Ok(list) => json_respond(&list),
                    Err(e) => storage_err(e),
                }
            }

            // --- Crypto ---
            "crypto.hash" => {
                let crypto = match require_service(&services.crypto, "crypto") {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                let password = String::from_utf8_lossy(&msg.data);
                match crypto.hash(&password) {
                    Ok(hash) => ok_respond(hash.into_bytes()),
                    Err(e) => err_result(ErrorCode::INTERNAL, e.to_string()),
                }
            }

            "crypto.compare_hash" => {
                let crypto = match require_service(&services.crypto, "crypto") {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                let password = String::from_utf8_lossy(&msg.data);
                let hash = msg.get_meta("hash");
                match crypto.compare_hash(&password, hash) {
                    Ok(()) => ok_respond(b"true".to_vec()), // match
                    Err(crate::services::crypto::CryptoError::PasswordMismatch) => ok_continue(), // no match
                    Err(e) => err_result(ErrorCode::INTERNAL, e.to_string()),
                }
            }

            "crypto.sign" => {
                let crypto = match require_service(&services.crypto, "crypto") {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                // Data contains JSON-encoded claims map
                let claims: HashMap<String, serde_json::Value> =
                    match serde_json::from_slice(&msg.data) {
                        Ok(c) => c,
                        Err(e) => {
                            return err_result(
                                ErrorCode::INVALID_ARGUMENT,
                                format!("invalid claims JSON: {e}"),
                            )
                        }
                    };
                // Expiry from meta (seconds), default 24h
                let expiry_secs: u64 = msg
                    .get_meta("expiry")
                    .parse()
                    .unwrap_or(86400);
                let expiry = Duration::from_secs(expiry_secs);
                match crypto.sign(claims, expiry) {
                    Ok(token) => ok_respond(token.into_bytes()),
                    Err(e) => err_result(ErrorCode::INTERNAL, e.to_string()),
                }
            }

            "crypto.verify" => {
                let crypto = match require_service(&services.crypto, "crypto") {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                let token = String::from_utf8_lossy(&msg.data);
                match crypto.verify(&token) {
                    Ok(claims) => json_respond(&claims),
                    Err(e) => err_result(ErrorCode::UNAUTHENTICATED, e.to_string()),
                }
            }

            "crypto.random_bytes" => {
                let crypto = match require_service(&services.crypto, "crypto") {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                let length: usize = msg
                    .get_meta("length")
                    .parse()
                    .unwrap_or(32);
                match crypto.random_bytes(length) {
                    Ok(bytes) => ok_respond(bytes),
                    Err(e) => err_result(ErrorCode::INTERNAL, e.to_string()),
                }
            }

            // --- Network ---
            "network.do" => {
                let network = match require_service(&services.network, "network") {
                    Ok(n) => n,
                    Err(e) => return e,
                };
                #[derive(Deserialize)]
                struct WireNetworkRequest {
                    method: String,
                    url: String,
                    #[serde(default)]
                    headers: HashMap<String, String>,
                    #[serde(default)]
                    body: Option<serde_json::Value>,
                }
                let wire_req: WireNetworkRequest = match serde_json::from_slice(&msg.data) {
                    Ok(r) => r,
                    Err(e) => {
                        return err_result(
                            ErrorCode::INVALID_ARGUMENT,
                            format!("invalid network request: {e}"),
                        )
                    }
                };
                // SSRF defense-in-depth: block private IPs and non-HTTP schemes
                if crate::security::is_blocked_url(&wire_req.url) {
                    return err_result(
                        ErrorCode::PERMISSION_DENIED,
                        format!(
                            "network request to {:?} blocked: private/internal address or disallowed scheme",
                            wire_req.url
                        ),
                    );
                }
                let body = match &wire_req.body {
                    Some(serde_json::Value::String(s)) => Some(s.as_bytes().to_vec()),
                    Some(serde_json::Value::Array(_)) | Some(serde_json::Value::Object(_)) => {
                        Some(serde_json::to_vec(&wire_req.body).unwrap_or_default())
                    }
                    _ => None,
                };
                let req = crate::services::network::Request {
                    method: wire_req.method,
                    url: wire_req.url,
                    headers: wire_req.headers,
                    body,
                };
                match network.do_request(&req) {
                    Ok(resp) => {
                        // Serialize response for the guest. Go expects body as base64 string
                        // for []byte fields.
                        let headers: HashMap<String, String> = resp
                            .headers
                            .into_iter()
                            .map(|(k, v)| (k, v.into_iter().next().unwrap_or_default()))
                            .collect();
                        let wire_resp = serde_json::json!({
                            "status_code": resp.status_code,
                            "headers": headers,
                            "body": base64_encode(&resp.body),
                        });
                        match serde_json::to_vec(&wire_resp) {
                            Ok(data) => ok_respond(data),
                            Err(e) => err_result(
                                ErrorCode::INTERNAL,
                                format!("failed to serialize network response: {e}"),
                            ),
                        }
                    }
                    Err(e) => err_result(ErrorCode::INTERNAL, e.to_string()),
                }
            }

            // --- Logger ---
            k if k.starts_with("logger.") => {
                let level = &k[7..]; // strip "logger."
                let message = String::from_utf8_lossy(&msg.data);
                match level {
                    "debug" => tracing::debug!(chain_id = %self.chain_id, node_id = %self.node_id, "{}", message),
                    "info" => tracing::info!(chain_id = %self.chain_id, node_id = %self.node_id, "{}", message),
                    "warn" => tracing::warn!(chain_id = %self.chain_id, node_id = %self.node_id, "{}", message),
                    "error" => tracing::error!(chain_id = %self.chain_id, node_id = %self.node_id, "{}", message),
                    _ => tracing::info!(chain_id = %self.chain_id, node_id = %self.node_id, level = %level, "{}", message),
                }
                ok_continue()
            }

            // --- Config ---
            "config.get" => {
                let config = match &services.config {
                    Some(c) => c,
                    None => {
                        // Fall back to block's node config
                        let key = msg.get_meta("key");
                        return match self.config.get(key) {
                            Some(val) => ok_respond(val.as_bytes().to_vec()),
                            None => err_result(
                                ErrorCode::NOT_FOUND,
                                format!("config key not found: {key}"),
                            ),
                        };
                    }
                };
                let key = msg.get_meta("key");
                match config.get(key) {
                    Some(val) => ok_respond(val.into_bytes()),
                    None => err_result(ErrorCode::NOT_FOUND, format!("config key not found: {key}")),
                }
            }

            "config.set" => {
                let config = match require_service(&services.config, "config") {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                let key = msg.get_meta("key");
                let value = String::from_utf8_lossy(&msg.data);
                config.set(key, &value);
                ok_empty()
            }

            _ => err_result(
                ErrorCode::UNAVAILABLE,
                format!("unknown service capability: svc.{svc_kind}"),
            ),
        }
    }
}
