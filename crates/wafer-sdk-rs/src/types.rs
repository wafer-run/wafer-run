//! Guest-side types for WAFER blocks.
//!
//! These types mirror the WIT-generated types but provide ergonomic Rust APIs
//! with HashMap-based metadata, helper methods, and conversions.

use std::collections::HashMap;
use std::fmt;

// Re-export WIT-generated types that block authors use directly.
pub use crate::wafer::block_world::types::{
    Action, BlockInfo, BlockResult, ErrorCode, InstanceMode, LifecycleEvent, LifecycleType,
    MetaEntry, Response, WaferError,
};

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

// Re-export Message as the WIT-generated type.
pub use crate::wafer::block_world::types::Message;

// ---------------------------------------------------------------------------
// Extension trait for Message
// ---------------------------------------------------------------------------

/// Extension methods for the WIT-generated Message type.
pub trait MessageExt {
    fn get_meta(&self, key: &str) -> &str;
    fn set_meta(&mut self, key: &str, value: &str);
    fn meta_map(&self) -> HashMap<String, String>;

    fn unmarshal<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error>;
    fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error>;
    fn set_data<T: serde::Serialize>(&mut self, v: &T) -> Result<(), serde_json::Error>;

    fn cont(self) -> BlockResult;
    fn respond_with(self, r: Response) -> BlockResult;
    fn drop_msg(self) -> BlockResult;
    fn err(self, e: WaferError) -> BlockResult;

    fn var(&self, name: &str) -> &str;
    fn query(&self, name: &str) -> &str;
    fn header(&self, name: &str) -> &str;
    fn action_str(&self) -> &str;
    fn path(&self) -> &str;
    fn content_type(&self) -> &str;
    fn user_id(&self) -> &str;
    fn user_email(&self) -> &str;
    fn user_roles(&self) -> Vec<&str>;
    fn is_admin(&self) -> bool;
    fn remote_addr(&self) -> &str;
    fn body(&self) -> &[u8];
    fn cookie(&self, name: &str) -> &str;
    fn query_params(&self) -> HashMap<&str, &str>;
    fn pagination_params(&self, default_page_size: usize) -> (usize, usize, usize);
}

impl MessageExt for Message {
    fn get_meta(&self, key: &str) -> &str {
        self.meta.iter()
            .find(|e| e.key == key)
            .map(|e| e.value.as_str())
            .unwrap_or("")
    }

    fn set_meta(&mut self, key: &str, value: &str) {
        if let Some(entry) = self.meta.iter_mut().find(|e| e.key == key) {
            entry.value = value.to_string();
        } else {
            self.meta.push(MetaEntry { key: key.to_string(), value: value.to_string() });
        }
    }

    fn meta_map(&self) -> HashMap<String, String> {
        self.meta.iter().map(|e| (e.key.clone(), e.value.clone())).collect()
    }

    fn unmarshal<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(&self.data)
    }

    fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        self.unmarshal()
    }

    fn set_data<T: serde::Serialize>(&mut self, v: &T) -> Result<(), serde_json::Error> {
        self.data = serde_json::to_vec(v)?;
        Ok(())
    }

    fn cont(self) -> BlockResult {
        BlockResult {
            action: Action::Continue,
            response: None,
            error: None,
            message: Some(self),
        }
    }

    fn respond_with(self, r: Response) -> BlockResult {
        BlockResult {
            action: Action::Respond,
            response: Some(r),
            error: None,
            message: Some(self),
        }
    }

    fn drop_msg(self) -> BlockResult {
        BlockResult {
            action: Action::Drop,
            response: None,
            error: None,
            message: Some(self),
        }
    }

    fn err(self, e: WaferError) -> BlockResult {
        BlockResult {
            action: Action::Error,
            response: None,
            error: Some(e),
            message: Some(self),
        }
    }

    fn var(&self, name: &str) -> &str {
        let key = format!("{}{}", META_REQ_PARAM_PREFIX, name);
        self.get_meta(&key)
    }

    fn query(&self, name: &str) -> &str {
        let key = format!("{}{}", META_REQ_QUERY_PREFIX, name);
        self.get_meta(&key)
    }

    fn header(&self, name: &str) -> &str {
        let key = format!("http.header.{}", name);
        self.get_meta(&key)
    }

    fn action_str(&self) -> &str {
        self.get_meta(META_REQ_ACTION)
    }

    fn path(&self) -> &str {
        self.get_meta(META_REQ_RESOURCE)
    }

    fn content_type(&self) -> &str {
        self.get_meta(META_REQ_CONTENT_TYPE)
    }

    fn user_id(&self) -> &str {
        self.get_meta(META_AUTH_USER_ID)
    }

    fn user_email(&self) -> &str {
        self.get_meta(META_AUTH_USER_EMAIL)
    }

    fn user_roles(&self) -> Vec<&str> {
        let roles = self.get_meta(META_AUTH_USER_ROLES);
        if roles.is_empty() {
            Vec::new()
        } else {
            roles.split(',').collect()
        }
    }

    fn is_admin(&self) -> bool {
        self.user_roles().contains(&"admin")
    }

    fn remote_addr(&self) -> &str {
        self.get_meta(META_REQ_CLIENT_IP)
    }

    fn body(&self) -> &[u8] {
        &self.data
    }

    fn cookie(&self, name: &str) -> &str {
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

    fn query_params(&self) -> HashMap<&str, &str> {
        self.meta.iter()
            .filter(|e| e.key.starts_with(META_REQ_QUERY_PREFIX))
            .map(|e| (&e.key[META_REQ_QUERY_PREFIX.len()..], e.value.as_str()))
            .collect()
    }

    fn pagination_params(&self, default_page_size: usize) -> (usize, usize, usize) {
        let page = self.query("page")
            .parse::<usize>()
            .ok()
            .filter(|&p| p > 0)
            .unwrap_or(1);

        let page_size = self.query("page_size")
            .parse::<usize>()
            .ok()
            .filter(|&ps| ps > 0 && ps <= 100)
            .unwrap_or(default_page_size);

        let offset = (page - 1) * page_size;
        (page, page_size, offset)
    }
}

// ---------------------------------------------------------------------------
// RequestAction (convenience enum, not in WIT)
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

/// Create a new message with the given kind and data.
pub fn new_message(kind: impl Into<String>, data: impl Into<Vec<u8>>) -> Message {
    Message {
        kind: kind.into(),
        data: data.into(),
        meta: Vec::new(),
    }
}

/// Create an error BlockResult.
pub fn error_result(code: ErrorCode, message: &str) -> BlockResult {
    BlockResult {
        action: Action::Error,
        response: None,
        error: Some(WaferError {
            code,
            message: message.to_string(),
            meta: Vec::new(),
        }),
        message: None,
    }
}
