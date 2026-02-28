use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::context::Context;
use crate::services;
use crate::wasm::capabilities::BlockCapabilities;
use super::bindings;

/// HostState stores the wafer Context and capabilities for host function calls.
pub struct HostState {
    pub context: Option<Arc<dyn Context>>,
    pub capabilities: BlockCapabilities,
}

impl HostState {
    fn services(&self) -> Option<&services::Services> {
        self.context.as_ref()?.services()
    }

    fn check_capability_deny(&self, service: &str, detail: &str) -> Option<String> {
        let caps = &self.capabilities;
        match service {
            "database" => {
                if detail == "query_raw" || detail == "exec_raw" {
                    if !caps.raw_sql {
                        return Some(format!("block not allowed to use {detail}: raw_sql not permitted"));
                    }
                } else if !detail.is_empty() && !caps.allows_collection(detail) {
                    return Some(format!("block not allowed to access collection {:?}", detail));
                }
                None
            }
            "storage" => {
                if !detail.is_empty() && !caps.allows_storage_folder(detail) {
                    return Some(format!("block not allowed to access storage folder {:?}", detail));
                }
                None
            }
            "crypto" => {
                if !caps.crypto {
                    return Some("block not allowed to use crypto service".to_string());
                }
                None
            }
            "network" => {
                if !caps.network {
                    return Some("block not allowed to use network service".to_string());
                }
                if !detail.is_empty() && !caps.allows_network_url(detail) {
                    return Some(format!("block not allowed to access URL {:?}", detail));
                }
                None
            }
            "config" => {
                if !caps.config {
                    return Some("block not allowed to use config service".to_string());
                }
                if !detail.is_empty() && !caps.allows_config_key(detail) {
                    return Some(format!("block not allowed to access config key {:?}", detail));
                }
                None
            }
            _ => None,
        }
    }

}


// ---------------------------------------------------------------------------
// Database host implementation
// ---------------------------------------------------------------------------

impl bindings::database::Host for HostState {
    fn get(&mut self, collection: String, id: String) -> Result<bindings::database::DbRecord, bindings::database::DatabaseError> {
        if let Some(msg) = self.check_capability_deny("database", &collection) {
            tracing::warn!("{}", msg);
            return Err(bindings::database::DatabaseError::Internal);
        }
        let db = self.services()
            .and_then(|s| s.database.as_ref())
            .ok_or(bindings::database::DatabaseError::Internal)?;
        match db.get(&collection, &id) {
            Ok(r) => Ok(bindings::database::DbRecord {
                id: r.id,
                data: serde_json::to_string(&r.data).unwrap_or_default(),
            }),
            Err(services::database::DatabaseError::NotFound) => Err(bindings::database::DatabaseError::NotFound),
            Err(_) => Err(bindings::database::DatabaseError::Internal),
        }
    }

    fn list(&mut self, collection: String, options: bindings::database::ListOptions) -> Result<bindings::database::RecordList, bindings::database::DatabaseError> {
        if let Some(msg) = self.check_capability_deny("database", &collection) {
            tracing::warn!("{}", msg);
            return Err(bindings::database::DatabaseError::Internal);
        }
        let db = self.services()
            .and_then(|s| s.database.as_ref())
            .ok_or(bindings::database::DatabaseError::Internal)?;
        let opts = convert_list_options(options);
        match db.list(&collection, &opts) {
            Ok(rl) => Ok(bindings::database::RecordList {
                records: rl.records.into_iter().map(|r| bindings::database::DbRecord {
                    id: r.id,
                    data: serde_json::to_string(&r.data).unwrap_or_default(),
                }).collect(),
                total_count: rl.total_count,
                page: rl.page,
                page_size: rl.page_size,
            }),
            Err(services::database::DatabaseError::NotFound) => Err(bindings::database::DatabaseError::NotFound),
            Err(_) => Err(bindings::database::DatabaseError::Internal),
        }
    }

    fn create(&mut self, collection: String, data: String) -> Result<bindings::database::DbRecord, bindings::database::DatabaseError> {
        if let Some(msg) = self.check_capability_deny("database", &collection) {
            tracing::warn!("{}", msg);
            return Err(bindings::database::DatabaseError::Internal);
        }
        let db = self.services()
            .and_then(|s| s.database.as_ref())
            .ok_or(bindings::database::DatabaseError::Internal)?;
        let map: HashMap<String, serde_json::Value> = serde_json::from_str(&data)
            .map_err(|_| bindings::database::DatabaseError::Internal)?;
        match db.create(&collection, map) {
            Ok(r) => Ok(bindings::database::DbRecord {
                id: r.id,
                data: serde_json::to_string(&r.data).unwrap_or_default(),
            }),
            Err(_) => Err(bindings::database::DatabaseError::Internal),
        }
    }

    fn update(&mut self, collection: String, id: String, data: String) -> Result<bindings::database::DbRecord, bindings::database::DatabaseError> {
        if let Some(msg) = self.check_capability_deny("database", &collection) {
            tracing::warn!("{}", msg);
            return Err(bindings::database::DatabaseError::Internal);
        }
        let db = self.services()
            .and_then(|s| s.database.as_ref())
            .ok_or(bindings::database::DatabaseError::Internal)?;
        let map: HashMap<String, serde_json::Value> = serde_json::from_str(&data)
            .map_err(|_| bindings::database::DatabaseError::Internal)?;
        match db.update(&collection, &id, map) {
            Ok(r) => Ok(bindings::database::DbRecord {
                id: r.id,
                data: serde_json::to_string(&r.data).unwrap_or_default(),
            }),
            Err(services::database::DatabaseError::NotFound) => Err(bindings::database::DatabaseError::NotFound),
            Err(_) => Err(bindings::database::DatabaseError::Internal),
        }
    }

    fn delete(&mut self, collection: String, id: String) -> Result<(), bindings::database::DatabaseError> {
        if let Some(msg) = self.check_capability_deny("database", &collection) {
            tracing::warn!("{}", msg);
            return Err(bindings::database::DatabaseError::Internal);
        }
        let db = self.services()
            .and_then(|s| s.database.as_ref())
            .ok_or(bindings::database::DatabaseError::Internal)?;
        match db.delete(&collection, &id) {
            Ok(()) => Ok(()),
            Err(services::database::DatabaseError::NotFound) => Err(bindings::database::DatabaseError::NotFound),
            Err(_) => Err(bindings::database::DatabaseError::Internal),
        }
    }

    fn count(&mut self, collection: String, filters: Vec<bindings::database::Filter>) -> Result<i64, bindings::database::DatabaseError> {
        if let Some(msg) = self.check_capability_deny("database", &collection) {
            tracing::warn!("{}", msg);
            return Err(bindings::database::DatabaseError::Internal);
        }
        let db = self.services()
            .and_then(|s| s.database.as_ref())
            .ok_or(bindings::database::DatabaseError::Internal)?;
        let native_filters: Vec<services::database::Filter> = filters.into_iter().map(convert_filter).collect();
        match db.count(&collection, &native_filters) {
            Ok(n) => Ok(n),
            Err(_) => Err(bindings::database::DatabaseError::Internal),
        }
    }

    fn query_raw(&mut self, query: String, args: String) -> Result<Vec<bindings::database::DbRecord>, bindings::database::DatabaseError> {
        if let Some(msg) = self.check_capability_deny("database", "query_raw") {
            tracing::warn!("{}", msg);
            return Err(bindings::database::DatabaseError::Internal);
        }
        let db = self.services()
            .and_then(|s| s.database.as_ref())
            .ok_or(bindings::database::DatabaseError::Internal)?;
        let parsed_args: Vec<serde_json::Value> = serde_json::from_str(&args).unwrap_or_default();
        match db.query_raw(&query, &parsed_args) {
            Ok(records) => Ok(records.into_iter().map(|r| bindings::database::DbRecord {
                id: r.id,
                data: serde_json::to_string(&r.data).unwrap_or_default(),
            }).collect()),
            Err(_) => Err(bindings::database::DatabaseError::Internal),
        }
    }

    fn exec_raw(&mut self, query: String, args: String) -> Result<i64, bindings::database::DatabaseError> {
        if let Some(msg) = self.check_capability_deny("database", "exec_raw") {
            tracing::warn!("{}", msg);
            return Err(bindings::database::DatabaseError::Internal);
        }
        let db = self.services()
            .and_then(|s| s.database.as_ref())
            .ok_or(bindings::database::DatabaseError::Internal)?;
        let parsed_args: Vec<serde_json::Value> = serde_json::from_str(&args).unwrap_or_default();
        match db.exec_raw(&query, &parsed_args) {
            Ok(n) => Ok(n),
            Err(_) => Err(bindings::database::DatabaseError::Internal),
        }
    }
}

fn convert_filter_op(op: bindings::database::FilterOp) -> services::database::FilterOp {
    match op {
        bindings::database::FilterOp::Eq => services::database::FilterOp::Equal,
        bindings::database::FilterOp::Neq => services::database::FilterOp::NotEqual,
        bindings::database::FilterOp::Gt => services::database::FilterOp::GreaterThan,
        bindings::database::FilterOp::Gte => services::database::FilterOp::GreaterEqual,
        bindings::database::FilterOp::Lt => services::database::FilterOp::LessThan,
        bindings::database::FilterOp::Lte => services::database::FilterOp::LessEqual,
        bindings::database::FilterOp::Like => services::database::FilterOp::Like,
        bindings::database::FilterOp::In => services::database::FilterOp::In,
        bindings::database::FilterOp::IsNull => services::database::FilterOp::IsNull,
        bindings::database::FilterOp::IsNotNull => services::database::FilterOp::IsNotNull,
    }
}

fn convert_filter(f: bindings::database::Filter) -> services::database::Filter {
    services::database::Filter {
        field: f.field,
        operator: convert_filter_op(f.operator),
        value: serde_json::from_str(&f.value).unwrap_or(serde_json::Value::Null),
    }
}

fn convert_list_options(opts: bindings::database::ListOptions) -> services::database::ListOptions {
    services::database::ListOptions {
        filters: opts.filters.into_iter().map(convert_filter).collect(),
        sort: opts.sort.into_iter().map(|s| services::database::SortField {
            field: s.field,
            desc: s.desc,
        }).collect(),
        limit: opts.limit,
        offset: opts.offset,
    }
}

// ---------------------------------------------------------------------------
// Storage host implementation
// ---------------------------------------------------------------------------

impl bindings::storage::Host for HostState {
    fn put(&mut self, folder: String, key: String, data: Vec<u8>, content_type: String) -> Result<(), bindings::storage::StorageError> {
        if let Some(msg) = self.check_capability_deny("storage", &folder) {
            tracing::warn!("{}", msg);
            return Err(bindings::storage::StorageError::Internal);
        }
        let storage = self.services()
            .and_then(|s| s.storage.as_ref())
            .ok_or(bindings::storage::StorageError::Internal)?;
        match storage.put(&folder, &key, &data, &content_type) {
            Ok(()) => Ok(()),
            Err(services::storage::StorageError::NotFound) => Err(bindings::storage::StorageError::NotFound),
            Err(_) => Err(bindings::storage::StorageError::Internal),
        }
    }

    fn get(&mut self, folder: String, key: String) -> Result<(Vec<u8>, bindings::storage::ObjectInfo), bindings::storage::StorageError> {
        if let Some(msg) = self.check_capability_deny("storage", &folder) {
            tracing::warn!("{}", msg);
            return Err(bindings::storage::StorageError::Internal);
        }
        let storage = self.services()
            .and_then(|s| s.storage.as_ref())
            .ok_or(bindings::storage::StorageError::Internal)?;
        match storage.get(&folder, &key) {
            Ok((data, info)) => Ok((data, bindings::storage::ObjectInfo {
                key: info.key,
                size: info.size,
                content_type: info.content_type,
                last_modified: info.last_modified.to_rfc3339(),
            })),
            Err(services::storage::StorageError::NotFound) => Err(bindings::storage::StorageError::NotFound),
            Err(_) => Err(bindings::storage::StorageError::Internal),
        }
    }

    fn delete(&mut self, folder: String, key: String) -> Result<(), bindings::storage::StorageError> {
        if let Some(msg) = self.check_capability_deny("storage", &folder) {
            tracing::warn!("{}", msg);
            return Err(bindings::storage::StorageError::Internal);
        }
        let storage = self.services()
            .and_then(|s| s.storage.as_ref())
            .ok_or(bindings::storage::StorageError::Internal)?;
        match storage.delete(&folder, &key) {
            Ok(()) => Ok(()),
            Err(services::storage::StorageError::NotFound) => Err(bindings::storage::StorageError::NotFound),
            Err(_) => Err(bindings::storage::StorageError::Internal),
        }
    }

    fn list(&mut self, folder: String, prefix: String, limit: i64, offset: i64) -> Result<bindings::storage::ObjectList, bindings::storage::StorageError> {
        if let Some(msg) = self.check_capability_deny("storage", &folder) {
            tracing::warn!("{}", msg);
            return Err(bindings::storage::StorageError::Internal);
        }
        let storage = self.services()
            .and_then(|s| s.storage.as_ref())
            .ok_or(bindings::storage::StorageError::Internal)?;
        let opts = services::storage::ListOptions { prefix, limit, offset };
        match storage.list(&folder, &opts) {
            Ok(ol) => Ok(bindings::storage::ObjectList {
                objects: ol.objects.into_iter().map(|o| bindings::storage::ObjectInfo {
                    key: o.key,
                    size: o.size,
                    content_type: o.content_type,
                    last_modified: o.last_modified.to_rfc3339(),
                }).collect(),
                total_count: ol.total_count,
            }),
            Err(services::storage::StorageError::NotFound) => Err(bindings::storage::StorageError::NotFound),
            Err(_) => Err(bindings::storage::StorageError::Internal),
        }
    }
}

// ---------------------------------------------------------------------------
// Crypto host implementation
// ---------------------------------------------------------------------------

impl bindings::crypto::Host for HostState {
    fn hash(&mut self, password: String) -> Result<String, bindings::crypto::CryptoError> {
        if let Some(msg) = self.check_capability_deny("crypto", "") {
            tracing::warn!("{}", msg);
            return Err(bindings::crypto::CryptoError::Other);
        }
        let crypto = self.services()
            .and_then(|s| s.crypto.as_ref())
            .ok_or(bindings::crypto::CryptoError::Other)?;
        crypto.hash(&password).map_err(|_| bindings::crypto::CryptoError::HashError)
    }

    fn compare_hash(&mut self, password: String, hash: String) -> Result<(), bindings::crypto::CryptoError> {
        if let Some(msg) = self.check_capability_deny("crypto", "") {
            tracing::warn!("{}", msg);
            return Err(bindings::crypto::CryptoError::Other);
        }
        let crypto = self.services()
            .and_then(|s| s.crypto.as_ref())
            .ok_or(bindings::crypto::CryptoError::Other)?;
        match crypto.compare_hash(&password, &hash) {
            Ok(()) => Ok(()),
            Err(services::crypto::CryptoError::PasswordMismatch) => Err(bindings::crypto::CryptoError::PasswordMismatch),
            Err(_) => Err(bindings::crypto::CryptoError::Other),
        }
    }

    fn sign(&mut self, claims: String, expiry_secs: u64) -> Result<String, bindings::crypto::CryptoError> {
        if let Some(msg) = self.check_capability_deny("crypto", "") {
            tracing::warn!("{}", msg);
            return Err(bindings::crypto::CryptoError::Other);
        }
        let crypto = self.services()
            .and_then(|s| s.crypto.as_ref())
            .ok_or(bindings::crypto::CryptoError::Other)?;
        let claims_map: HashMap<String, serde_json::Value> = serde_json::from_str(&claims)
            .map_err(|_| bindings::crypto::CryptoError::SignError)?;
        crypto.sign(claims_map, Duration::from_secs(expiry_secs))
            .map_err(|_| bindings::crypto::CryptoError::SignError)
    }

    fn verify(&mut self, token: String) -> Result<String, bindings::crypto::CryptoError> {
        if let Some(msg) = self.check_capability_deny("crypto", "") {
            tracing::warn!("{}", msg);
            return Err(bindings::crypto::CryptoError::Other);
        }
        let crypto = self.services()
            .and_then(|s| s.crypto.as_ref())
            .ok_or(bindings::crypto::CryptoError::Other)?;
        match crypto.verify(&token) {
            Ok(claims) => serde_json::to_string(&claims).map_err(|_| bindings::crypto::CryptoError::VerifyError),
            Err(_) => Err(bindings::crypto::CryptoError::VerifyError),
        }
    }

    fn random_bytes(&mut self, n: u32) -> Result<Vec<u8>, bindings::crypto::CryptoError> {
        if let Some(msg) = self.check_capability_deny("crypto", "") {
            tracing::warn!("{}", msg);
            return Err(bindings::crypto::CryptoError::Other);
        }
        let crypto = self.services()
            .and_then(|s| s.crypto.as_ref())
            .ok_or(bindings::crypto::CryptoError::Other)?;
        crypto.random_bytes(n as usize).map_err(|_| bindings::crypto::CryptoError::Other)
    }
}

// ---------------------------------------------------------------------------
// Network host implementation
// ---------------------------------------------------------------------------

impl bindings::network::Host for HostState {
    fn do_request(&mut self, req: bindings::network::HttpRequest) -> Result<bindings::network::HttpResponse, bindings::network::NetworkError> {
        if let Some(msg) = self.check_capability_deny("network", &req.url) {
            tracing::warn!("{}", msg);
            return Err(bindings::network::NetworkError::Other);
        }
        if crate::security::is_blocked_url(&req.url) {
            return Err(bindings::network::NetworkError::SsrfBlocked);
        }
        let network = self.services()
            .and_then(|s| s.network.as_ref())
            .ok_or(bindings::network::NetworkError::Other)?;
        let headers: HashMap<String, String> = req.headers.into_iter()
            .map(|e| (e.key, e.value))
            .collect();
        let native_req = services::network::Request {
            method: req.method,
            url: req.url,
            headers,
            body: req.body,
        };
        match network.do_request(&native_req) {
            Ok(resp) => Ok(bindings::network::HttpResponse {
                status_code: resp.status_code,
                headers: resp.headers.into_iter()
                    .map(|(k, v)| bindings::types::MetaEntry {
                        key: k,
                        value: v.into_iter().next().unwrap_or_default(),
                    })
                    .collect(),
                body: resp.body,
            }),
            Err(_) => Err(bindings::network::NetworkError::RequestError),
        }
    }
}

// ---------------------------------------------------------------------------
// Logger host implementation
// ---------------------------------------------------------------------------

impl bindings::logger::Host for HostState {
    fn debug(&mut self, msg: String, fields: Vec<bindings::logger::LogField>) {
        let fields_str = format_log_fields(&fields);
        tracing::debug!("{} {}", msg, fields_str);
    }

    fn info(&mut self, msg: String, fields: Vec<bindings::logger::LogField>) {
        let fields_str = format_log_fields(&fields);
        tracing::info!("{} {}", msg, fields_str);
    }

    fn warn(&mut self, msg: String, fields: Vec<bindings::logger::LogField>) {
        let fields_str = format_log_fields(&fields);
        tracing::warn!("{} {}", msg, fields_str);
    }

    fn error(&mut self, msg: String, fields: Vec<bindings::logger::LogField>) {
        let fields_str = format_log_fields(&fields);
        tracing::error!("{} {}", msg, fields_str);
    }
}

fn format_log_fields(fields: &[bindings::logger::LogField]) -> String {
    if fields.is_empty() {
        return String::new();
    }
    fields.iter()
        .map(|f| format!("{}={}", f.key, f.value))
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Config host implementation
// ---------------------------------------------------------------------------

impl bindings::config::Host for HostState {
    fn get(&mut self, key: String) -> Option<String> {
        if let Some(msg) = self.check_capability_deny("config", &key) {
            tracing::warn!("{}", msg);
            return None;
        }
        let services = self.services()?;
        if let Some(config) = &services.config {
            return config.get(&key);
        }
        // Fall back to block's node config via context
        self.context.as_ref()
            .and_then(|ctx| ctx.config_get(&key))
            .map(|s| s.to_string())
    }

    fn set(&mut self, key: String, value: String) {
        if let Some(msg) = self.check_capability_deny("config", &key) {
            tracing::warn!("{}", msg);
            return;
        }
        if let Some(services) = self.services() {
            if let Some(config) = &services.config {
                config.set(&key, &value);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Runtime host implementation
// ---------------------------------------------------------------------------

impl bindings::runtime::Host for HostState {
    fn is_cancelled(&mut self) -> bool {
        match &self.context {
            Some(ctx) => ctx.is_cancelled(),
            None => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Types host implementation (needed because types is imported too)
// ---------------------------------------------------------------------------

impl bindings::types::Host for HostState {}
