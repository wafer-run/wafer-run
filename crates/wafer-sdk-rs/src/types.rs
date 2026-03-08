//! Guest-side types for WAFER blocks.
//!
//! These types are serialized as JSON over the thin ABI boundary.
//! They mirror the host-side types in `wafer-run::types`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

// ---------------------------------------------------------------------------
// Meta key constants (mirrors host meta.rs)
// ---------------------------------------------------------------------------

pub const META_REQ_ACTION: &str = "req.action";
pub const META_REQ_RESOURCE: &str = "req.resource";
pub const META_REQ_PARAM_PREFIX: &str = "req.param.";
pub const META_REQ_QUERY_PREFIX: &str = "req.query.";
pub const META_REQ_CLIENT_IP: &str = "req.client.ip";
pub const META_REQ_CONTENT_TYPE: &str = "req.content_type";

pub const META_AUTH_USER_ID: &str = "auth.user_id";
pub const META_AUTH_USER_EMAIL: &str = "auth.user_email";
pub const META_AUTH_USER_ROLES: &str = "auth.user_roles";

pub const META_RESP_STATUS: &str = "resp.status";
pub const META_RESP_CONTENT_TYPE: &str = "resp.content_type";
pub const META_RESP_HEADER_PREFIX: &str = "resp.header.";
pub const META_RESP_COOKIE_PREFIX: &str = "resp.set_cookie.";

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub kind: String,
    #[serde(default)]
    pub data: Vec<u8>,
    #[serde(default)]
    pub meta: HashMap<String, String>,
}

impl Message {
    pub fn new(kind: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self {
            kind: kind.into(),
            data: data.into(),
            meta: HashMap::new(),
        }
    }

    pub fn get_meta(&self, key: &str) -> &str {
        self.meta.get(key).map(|s| s.as_str()).unwrap_or("")
    }

    pub fn set_meta(&mut self, key: &str, value: &str) {
        self.meta.insert(key.to_string(), value.to_string());
    }

    pub fn meta_map(&self) -> &HashMap<String, String> {
        &self.meta
    }

    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(&self.data)
    }

    pub fn set_data<T: serde::Serialize>(&mut self, v: &T) -> Result<(), serde_json::Error> {
        self.data = serde_json::to_vec(v)?;
        Ok(())
    }

    pub fn cont(self) -> BlockResult {
        BlockResult {
            action: Action::Continue,
            response: None,
            error: None,
            message: Some(self),
        }
    }

    pub fn respond_with(self, r: Response) -> BlockResult {
        BlockResult {
            action: Action::Respond,
            response: Some(r),
            error: None,
            message: Some(self),
        }
    }

    pub fn drop_msg(self) -> BlockResult {
        BlockResult {
            action: Action::Drop,
            response: None,
            error: None,
            message: Some(self),
        }
    }

    pub fn err(self, e: WaferError) -> BlockResult {
        BlockResult {
            action: Action::Error,
            response: None,
            error: Some(e),
            message: Some(self),
        }
    }

    pub fn var(&self, name: &str) -> &str {
        let key = format!("{}{}", META_REQ_PARAM_PREFIX, name);
        self.meta.get(&key).map(|s| s.as_str()).unwrap_or("")
    }

    pub fn query(&self, name: &str) -> &str {
        let key = format!("{}{}", META_REQ_QUERY_PREFIX, name);
        self.meta.get(&key).map(|s| s.as_str()).unwrap_or("")
    }

    pub fn header(&self, name: &str) -> &str {
        let key = format!("http.header.{}", name);
        self.meta.get(&key).map(|s| s.as_str()).unwrap_or("")
    }

    pub fn action_str(&self) -> &str {
        self.get_meta(META_REQ_ACTION)
    }

    pub fn path(&self) -> &str {
        self.get_meta(META_REQ_RESOURCE)
    }

    pub fn content_type(&self) -> &str {
        self.get_meta(META_REQ_CONTENT_TYPE)
    }

    pub fn user_id(&self) -> &str {
        self.get_meta(META_AUTH_USER_ID)
    }

    pub fn user_email(&self) -> &str {
        self.get_meta(META_AUTH_USER_EMAIL)
    }

    pub fn user_roles(&self) -> Vec<&str> {
        let roles = self.get_meta(META_AUTH_USER_ROLES);
        if roles.is_empty() {
            Vec::new()
        } else {
            roles.split(',').collect()
        }
    }

    pub fn is_admin(&self) -> bool {
        self.user_roles().contains(&"admin")
    }

    pub fn remote_addr(&self) -> &str {
        self.get_meta(META_REQ_CLIENT_IP)
    }

    pub fn body(&self) -> &[u8] {
        &self.data
    }

    pub fn cookie(&self, name: &str) -> &str {
        let raw = self.get_meta("http.header.Cookie");
        if raw.is_empty() {
            return "";
        }
        for part in raw.split(';') {
            let part = part.trim();
            if let Some(eq) = part.find('=') {
                if &part[..eq] == name {
                    return &part[eq + 1..];
                }
            }
        }
        ""
    }

    pub fn query_params(&self) -> HashMap<&str, &str> {
        self.meta
            .iter()
            .filter(|(k, _)| k.starts_with(META_REQ_QUERY_PREFIX))
            .map(|(k, v)| (&k[META_REQ_QUERY_PREFIX.len()..], v.as_str()))
            .collect()
    }

    pub fn pagination_params(&self, default_page_size: usize) -> (usize, usize, usize) {
        let page = self
            .query("page")
            .parse::<usize>()
            .ok()
            .filter(|&p| p > 0)
            .unwrap_or(1);

        let page_size = self
            .query("page_size")
            .parse::<usize>()
            .ok()
            .filter(|&ps| ps > 0 && ps <= 100)
            .unwrap_or(default_page_size);

        let offset = (page - 1) * page_size;
        (page, page_size, offset)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Action {
    #[serde(rename = "continue")]
    Continue,
    #[serde(rename = "respond")]
    Respond,
    #[serde(rename = "drop")]
    Drop,
    #[serde(rename = "error")]
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    #[serde(default)]
    pub data: Vec<u8>,
    #[serde(default)]
    pub meta: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaferError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub meta: HashMap<String, String>,
}

impl WaferError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            meta: HashMap::new(),
        }
    }
}

impl fmt::Display for WaferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for WaferError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockResult {
    pub action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<Response>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<WaferError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InstanceMode {
    #[serde(rename = "per-node")]
    PerNode,
    #[serde(rename = "singleton")]
    Singleton,
    #[serde(rename = "per-flow")]
    PerFlow,
    #[serde(rename = "per-execution")]
    PerExecution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BlockRuntime {
    #[serde(rename = "native")]
    Native,
    #[serde(rename = "wasm")]
    Wasm,
}

impl Default for BlockRuntime {
    fn default() -> Self {
        Self::Wasm
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminUIInfo {
    pub path: String,
    pub icon: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockInfo {
    pub name: String,
    pub version: String,
    pub interface: String,
    pub summary: String,
    pub instance_mode: InstanceMode,
    #[serde(default)]
    pub allowed_modes: Vec<InstanceMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admin_ui: Option<AdminUIInfo>,
    #[serde(default)]
    pub runtime: BlockRuntime,
    #[serde(default)]
    pub requires: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleType {
    #[serde(rename = "init")]
    Init,
    #[serde(rename = "start")]
    Start,
    #[serde(rename = "stop")]
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleEvent {
    pub event_type: LifecycleType,
    #[serde(default)]
    pub data: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Error code constants (matching host ErrorCode)
// ---------------------------------------------------------------------------

pub const ERROR_OK: &str = "ok";
pub const ERROR_CANCELLED: &str = "cancelled";
pub const ERROR_UNKNOWN: &str = "unknown";
pub const ERROR_INVALID_ARGUMENT: &str = "invalid_argument";
pub const ERROR_NOT_FOUND: &str = "not_found";
pub const ERROR_ALREADY_EXISTS: &str = "already_exists";
pub const ERROR_PERMISSION_DENIED: &str = "permission_denied";
pub const ERROR_UNAUTHENTICATED: &str = "unauthenticated";
pub const ERROR_RESOURCE_EXHAUSTED: &str = "resource_exhausted";
pub const ERROR_UNIMPLEMENTED: &str = "unimplemented";
pub const ERROR_INTERNAL: &str = "internal";
pub const ERROR_UNAVAILABLE: &str = "unavailable";

// ---------------------------------------------------------------------------
// RequestAction (convenience enum)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestAction {
    Retrieve,
    Create,
    Update,
    Delete,
    Execute,
}

impl RequestAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Retrieve => "retrieve",
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Execute => "execute",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "retrieve" => Some(Self::Retrieve),
            "create" => Some(Self::Create),
            "update" => Some(Self::Update),
            "delete" => Some(Self::Delete),
            "execute" => Some(Self::Execute),
            _ => None,
        }
    }
}

impl fmt::Display for RequestAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Helper constructors
// ---------------------------------------------------------------------------

pub fn new_message(kind: impl Into<String>, data: impl Into<Vec<u8>>) -> Message {
    Message::new(kind, data)
}

pub fn error_result(code: &str, message: &str) -> BlockResult {
    BlockResult {
        action: Action::Error,
        response: None,
        error: Some(WaferError::new(code, message)),
        message: None,
    }
}
