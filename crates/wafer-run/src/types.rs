use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

use crate::meta::*;

/// Message flows through the chain. A message contains a kind identifier,
/// payload data, and metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub kind: String,
    #[serde(with = "serde_bytes_or_default")]
    pub data: Vec<u8>,
    #[serde(default)]
    pub meta: HashMap<String, String>,
}

mod serde_bytes_or_default {
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Accept either bytes, a sequence, or a string
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum BytesOrString {
            Bytes(Vec<u8>),
            Str(String),
        }

        match BytesOrString::deserialize(deserializer) {
            Ok(BytesOrString::Bytes(b)) => Ok(b),
            Ok(BytesOrString::Str(s)) => Ok(s.into_bytes()),
            Err(_) => Ok(Vec::new()),
        }
    }
}

impl Message {
    /// Create a new message with the given kind and data.
    pub fn new(kind: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self {
            kind: kind.into(),
            data: data.into(),
            meta: HashMap::new(),
        }
    }

    /// Unmarshal parses Data into the given type.
    pub fn unmarshal<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(&self.data)
    }

    /// Decode unmarshals the JSON body into dest.
    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(&self.data)
    }

    /// GetMeta returns a Meta value by key.
    pub fn get_meta(&self, key: &str) -> &str {
        self.meta.get(key).map(|s| s.as_str()).unwrap_or("")
    }

    /// SetMeta sets a Meta key-value pair.
    pub fn set_meta(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.meta.insert(key.into(), value.into());
    }

    /// SetData marshals v as JSON and sets it as Data.
    pub fn set_data<T: Serialize>(&mut self, v: &T) -> Result<(), serde_json::Error> {
        self.data = serde_json::to_vec(v)?;
        Ok(())
    }

    /// Continue returns a Result that passes the message to the next block.
    pub fn cont(self) -> Result_ {
        Result_ {
            action: Action::Continue,
            response: None,
            error: None,
            message: Some(self),
        }
    }

    /// Respond returns a Result that short-circuits the chain with a response.
    pub fn respond(self, r: Response) -> Result_ {
        Result_ {
            action: Action::Respond,
            response: Some(r),
            error: None,
            message: Some(self),
        }
    }

    /// Drop returns a Result that ends the chain silently.
    pub fn drop_msg(self) -> Result_ {
        Result_ {
            action: Action::Drop,
            response: None,
            error: None,
            message: Some(self),
        }
    }

    /// Err returns a Result that short-circuits the chain with an error.
    pub fn err(self, e: WaferError) -> Result_ {
        Result_ {
            action: Action::Error,
            response: None,
            error: Some(e),
            message: Some(self),
        }
    }

    /// Var returns a path variable extracted by the router.
    pub fn var(&self, name: &str) -> &str {
        let key = format!("{}{}", META_REQ_PARAM_PREFIX, name);
        self.meta.get(&key).map(|s| s.as_str()).unwrap_or("")
    }

    /// Query returns a query parameter.
    pub fn query(&self, name: &str) -> &str {
        let key = format!("{}{}", META_REQ_QUERY_PREFIX, name);
        self.meta.get(&key).map(|s| s.as_str()).unwrap_or("")
    }

    /// Header returns a request header value.
    pub fn header(&self, name: &str) -> &str {
        // Try exact case first, then lowercase (axum normalizes header names to lowercase)
        let key = format!("http.header.{}", name);
        if let Some(v) = self.meta.get(&key) {
            return v.as_str();
        }
        let key_lower = format!("http.header.{}", name.to_lowercase());
        self.meta.get(&key_lower).map(|s| s.as_str()).unwrap_or("")
    }

    /// Action returns the semantic request action.
    pub fn action(&self) -> &str {
        self.get_meta(META_REQ_ACTION)
    }

    /// Path returns the request resource path.
    pub fn path(&self) -> &str {
        self.get_meta(META_REQ_RESOURCE)
    }

    /// ContentType returns the request content type.
    pub fn content_type(&self) -> &str {
        self.get_meta(META_REQ_CONTENT_TYPE)
    }

    /// UserID returns the authenticated user's ID.
    pub fn user_id(&self) -> &str {
        self.get_meta(META_AUTH_USER_ID)
    }

    /// UserEmail returns the authenticated user's email.
    pub fn user_email(&self) -> &str {
        self.get_meta(META_AUTH_USER_EMAIL)
    }

    /// UserRoles returns the authenticated user's roles.
    pub fn user_roles(&self) -> Vec<&str> {
        let roles = self.get_meta(META_AUTH_USER_ROLES);
        if roles.is_empty() {
            Vec::new()
        } else {
            roles.split(',').collect()
        }
    }

    /// IsAdmin returns true if the authenticated user has the "admin" role.
    pub fn is_admin(&self) -> bool {
        self.user_roles().contains(&"admin")
    }

    /// QueryParams returns all query parameters as a map.
    pub fn query_params(&self) -> HashMap<&str, &str> {
        self.meta
            .iter()
            .filter(|(k, _)| k.starts_with(META_REQ_QUERY_PREFIX))
            .map(|(k, v)| (&k[META_REQ_QUERY_PREFIX.len()..], v.as_str()))
            .collect()
    }

    /// Cookie returns a named cookie value from the Cookie header.
    pub fn cookie(&self, name: &str) -> &str {
        // Try both cases: axum normalizes to lowercase
        let mut raw = self.get_meta("http.header.Cookie");
        if raw.is_empty() {
            raw = self.get_meta("http.header.cookie");
        }
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

    /// RemoteAddr returns the client's remote address.
    pub fn remote_addr(&self) -> &str {
        self.get_meta(META_REQ_CLIENT_IP)
    }

    /// Body returns the raw request body.
    pub fn body(&self) -> &[u8] {
        &self.data
    }

    /// PaginationParams extracts page/pageSize/offset from query params.
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

/// RequestAction represents a semantic request action (transport-agnostic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RequestAction {
    #[serde(rename = "retrieve")]
    Retrieve,
    #[serde(rename = "create")]
    Create,
    #[serde(rename = "update")]
    Update,
    #[serde(rename = "delete")]
    Delete,
    #[serde(rename = "execute")]
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
}

impl fmt::Display for RequestAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Action tells the runtime what to do after a block processes a message.
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

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Continue => f.write_str("continue"),
            Self::Respond => f.write_str("respond"),
            Self::Drop => f.write_str("drop"),
            Self::Error => f.write_str("error"),
        }
    }
}

/// Response carries data back to the caller when a block short-circuits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    #[serde(default)]
    pub data: Vec<u8>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub meta: HashMap<String, String>,
}

/// WaferError represents a structured error returned by a block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaferError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub meta: HashMap<String, String>,
}

impl fmt::Display for WaferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for WaferError {}

impl WaferError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            meta: HashMap::new(),
        }
    }

    /// WithMeta returns a copy with the given meta key-value added.
    pub fn with_meta(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.meta.insert(key.into(), value.into());
        self
    }
}

/// Result_ is the outcome of a block processing a message.
/// Named Result_ to avoid conflict with std::result::Result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Result_ {
    pub action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<Response>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<WaferError>,
    #[serde(skip)]
    pub message: Option<Message>,
}

impl Result_ {
    pub fn continue_with(msg: Message) -> Self {
        Self {
            action: Action::Continue,
            response: None,
            error: None,
            message: Some(msg),
        }
    }

    pub fn error(err: WaferError) -> Self {
        Self {
            action: Action::Error,
            response: None,
            error: Some(err),
            message: None,
        }
    }
}

/// InstanceMode controls how many block instances are created and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InstanceMode {
    #[serde(rename = "per-node")]
    PerNode,
    #[serde(rename = "singleton")]
    Singleton,
    #[serde(rename = "per-chain")]
    PerChain,
    #[serde(rename = "per-execution")]
    PerExecution,
}

impl InstanceMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PerNode => "per-node",
            Self::Singleton => "singleton",
            Self::PerChain => "per-chain",
            Self::PerExecution => "per-execution",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "per-node" | "" => Some(Self::PerNode),
            "singleton" => Some(Self::Singleton),
            "per-chain" => Some(Self::PerChain),
            "per-execution" => Some(Self::PerExecution),
            _ => None,
        }
    }
}

impl fmt::Display for InstanceMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// LifecycleType identifies the kind of lifecycle event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleType {
    Init,
    Start,
    Stop,
}

/// LifecycleEvent is sent to blocks during lifecycle transitions.
#[derive(Debug, Clone)]
pub struct LifecycleEvent {
    pub event_type: LifecycleType,
    pub data: Vec<u8>,
}
