//! Runtime-only types and convenience impls on core types.
//!
//! Core types (`Message`, `MetaEntry`, `ErrorCode`, `WaferError`) are defined
//! in `core_types`. This module adds:
//!
//! - Runtime-only types (`BlockInfo`, `AdminUIInfo`, `BlockRuntime`, `RequestAction`)
//! - Ergonomic constructors and helpers on core types
//! - Backward-compatible `SCREAMING_CASE` constants on `ErrorCode`

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Runtime-only types (not part of WIT)
// ---------------------------------------------------------------------------

/// HTTP method for block endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HttpMethod {
    #[serde(rename = "GET")]
    Get,
    #[serde(rename = "POST")]
    Post,
    #[serde(rename = "PATCH")]
    Patch,
    #[serde(rename = "DELETE")]
    Delete,
}

impl std::fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Get => f.write_str("GET"),
            Self::Post => f.write_str("POST"),
            Self::Patch => f.write_str("PATCH"),
            Self::Delete => f.write_str("DELETE"),
        }
    }
}

/// Access level required for a block endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthLevel {
    #[default]
    Public,
    Authenticated,
    Admin,
}

impl std::fmt::Display for AuthLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Public => f.write_str("public"),
            Self::Authenticated => f.write_str("authenticated"),
            Self::Admin => f.write_str("admin"),
        }
    }
}

/// Block category — determines how the block is displayed in the admin UI.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BlockCategory {
    /// User/admin-facing feature blocks (Auth, Products, Files, etc.)
    Feature,
    /// Internal service blocks (Database, Storage, Config, etc.)
    Service,
    /// System infrastructure and middleware (HTTP Listener, Router, CORS, etc.)
    Infrastructure,
    /// Uncategorized / third-party blocks.
    #[default]
    Misc,
}

/// Whether a block runs as native code or as a sandboxed WASM component.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BlockRuntime {
    #[default]
    Native,
    Wasm,
}

// ---------------------------------------------------------------------------
// WRAP — Resource access grants
// ---------------------------------------------------------------------------

/// Resource type for typed grants. `None` matches any type.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceType {
    /// Database collections (get, list, create, update, delete)
    Db,
    /// Configuration keys (get, set)
    Config,
    /// Storage folders and files (get, put, delete, list)
    Storage,
    /// Per-block crypto key namespaces (sign, verify)
    Crypto,
    /// Outbound network access (HTTP requests)
    Network,
}

impl std::fmt::Display for ResourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db => f.write_str("db"),
            Self::Config => f.write_str("config"),
            Self::Storage => f.write_str("storage"),
            Self::Crypto => f.write_str("crypto"),
            Self::Network => f.write_str("network"),
        }
    }
}

impl ResourceType {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "db" => Some(Self::Db),
            "config" => Some(Self::Config),
            "storage" => Some(Self::Storage),
            "crypto" => Some(Self::Crypto),
            "network" => Some(Self::Network),
            _ => None,
        }
    }
}

/// A resource access grant declared by a block.
///
/// Blocks can only grant access to resources they own (enforced at startup).
/// The runtime collects all grants and checks them in `call_block()`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResourceGrant {
    /// Block ID that receives this grant, or `"*"` for all blocks.
    pub grantee: String,
    /// Exact resource name or prefix pattern ending with `*`.
    pub resource: String,
    /// If true, the grantee can both read and write. If false, read-only.
    #[serde(default)]
    pub write: bool,
    /// Resource type this grant applies to. `None` = all types (wildcard).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<ResourceType>,
}

impl ResourceGrant {
    /// Create a read-only grant (all resource types).
    pub fn read(grantee: &str, resource: &str) -> Self {
        Self {
            grantee: grantee.to_string(),
            resource: resource.to_string(),
            write: false,
            resource_type: None,
        }
    }

    /// Create a read-write grant (all resource types).
    pub fn read_write(grantee: &str, resource: &str) -> Self {
        Self {
            grantee: grantee.to_string(),
            resource: resource.to_string(),
            write: true,
            resource_type: None,
        }
    }

    /// Restrict this grant to a specific resource type.
    pub fn typed(mut self, rt: ResourceType) -> Self {
        self.resource_type = Some(rt);
        self
    }
}

fn default_true() -> bool {
    true
}
fn default_instance_mode() -> crate::InstanceMode {
    crate::InstanceMode::PerNode
}

/// Block metadata — identity, schema declarations, and admin UI metadata.
///
/// Only `name`, `version`, `interface`, and `summary` are required.
/// All other fields have sensible defaults via `Default`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BlockInfo {
    // -- Core identity --
    pub name: String,
    pub version: String,
    pub interface: String,
    pub summary: String,
    #[serde(default = "default_instance_mode")]
    pub instance_mode: crate::InstanceMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,

    // -- Schema declarations --
    /// Database collections this block requires. The runtime ensures these
    /// tables exist when the block is registered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collections: Vec<CollectionSchema>,
    /// Process env-var-style config variables declared by this block.
    /// **Keys must be SCREAMING_SNAKE with the block's `{ORG}__{BLOCK}__`
    /// prefix** (e.g., `WAFER_RUN__NETWORK__MAX_RESPONSE_BYTES`). Read by
    /// blocks via `std::env::var(KEY)`.
    ///
    /// For per-flow-step JSON config (snake_case keys read via
    /// `BlockConfig::from_event` or `ctx.config_get`), use
    /// [`Self::flow_config`] instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_keys: Vec<ConfigVar>,
    /// Per-flow-step JSON config keys this block reads via
    /// `BlockConfig::from_event` or `ctx.config_get`. **Keys must be
    /// snake_case identifiers** (e.g., `listen`, `allowed_origins`).
    ///
    /// This is distinct from [`Self::config_keys`], which declares
    /// process env-var-style keys (SCREAMING_SNAKE, `{ORG}__{BLOCK}__`
    /// prefix). A workspace validator asserts the two slots don't
    /// overlap and that each entry obeys its slot's naming convention.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flow_config: Vec<ConfigVar>,
    /// WRAP resource access grants declared by this block.
    /// Blocks can only grant access to resources they own (enforced at startup).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<ResourceGrant>,

    // -- Admin / UI metadata --
    /// Block category for admin UI grouping.
    #[serde(default)]
    pub category: BlockCategory,
    /// Whether this block runs as native code or WASM.
    #[serde(default)]
    pub runtime: BlockRuntime,
    /// Whether this block can be disabled by the admin.
    #[serde(default)]
    pub can_disable: bool,
    /// Whether the block is enabled by default on first run.
    #[serde(default = "default_true")]
    pub default_enabled: bool,
    /// Longer description of what the block does and how to use it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// HTTP endpoints exposed by this block.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<BlockEndpoint>,
    /// URL path to the block's admin UI (e.g., `/b/products/admin/`).
    /// Empty if the block has no admin UI.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub admin_url: String,

    /// Capability declaration.
    ///
    /// For WASM blocks: carried in the JSON returned by `__wafer_info` and
    /// intersected with operator config at load time. Enforced at dispatch.
    ///
    /// For native blocks: documentation and inspector metadata only. Not
    /// enforced by the runtime. Native blocks continue to operate under
    /// the existing trust model.
    ///
    /// `None` means the block did not declare — the runtime applies the
    /// existing default for that block's runtime type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<crate::BlockCapabilities>,

    // -- Skill / agent metadata --
    /// Whether this block acts as an agent-callable tool.
    /// `None` means the block is not exposed as a skill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<SkillRole>,

    /// OpenAI-compatible tool descriptor for this block when `role == Some(Skill)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<SkillTool>,

    /// Heavy external WASM/JS assets the host must load lazily before this block runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_assets: Vec<ExternalAsset>,
}

impl Default for BlockInfo {
    fn default() -> Self {
        Self::new("", "", "", "")
    }
}

impl BlockInfo {
    /// Create a new BlockInfo with the four required fields.
    /// All other fields use sensible defaults.
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        interface: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            interface: interface.into(),
            summary: summary.into(),
            instance_mode: crate::InstanceMode::PerNode,
            requires: Vec::new(),
            collections: Vec::new(),
            config_keys: Vec::new(),
            flow_config: Vec::new(),
            grants: Vec::new(),
            category: BlockCategory::default(),
            runtime: BlockRuntime::default(),
            can_disable: false,
            default_enabled: true,
            description: String::new(),
            endpoints: Vec::new(),
            admin_url: String::new(),
            capabilities: None,
            role: None,
            tool: None,
            external_assets: Vec::new(),
        }
    }

    pub fn instance_mode(mut self, mode: crate::InstanceMode) -> Self {
        self.instance_mode = mode;
        self
    }

    pub fn requires(mut self, requires: Vec<String>) -> Self {
        self.requires = requires;
        self
    }

    pub fn collections(mut self, collections: Vec<CollectionSchema>) -> Self {
        self.collections = collections;
        self
    }

    /// Set the per-flow-step JSON config keys this block reads. See the
    /// field doc on [`Self::flow_config`].
    pub fn flow_config(mut self, flow_config: Vec<ConfigVar>) -> Self {
        self.flow_config = flow_config;
        self
    }

    pub fn config_keys(mut self, config_keys: Vec<BlockConfigKey>) -> Self {
        self.config_keys = config_keys;
        self
    }

    pub fn grants(mut self, grants: Vec<ResourceGrant>) -> Self {
        self.grants = grants;
        self
    }

    pub fn category(mut self, category: BlockCategory) -> Self {
        self.category = category;
        self
    }

    pub fn runtime(mut self, runtime: BlockRuntime) -> Self {
        self.runtime = runtime;
        self
    }

    pub fn can_disable(mut self, can_disable: bool) -> Self {
        self.can_disable = can_disable;
        self
    }

    pub fn default_enabled(mut self, default_enabled: bool) -> Self {
        self.default_enabled = default_enabled;
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn endpoints(mut self, endpoints: Vec<BlockEndpoint>) -> Self {
        self.endpoints = endpoints;
        self
    }

    pub fn admin_url(mut self, url: impl Into<String>) -> Self {
        self.admin_url = url.into();
        self
    }

    /// Set the block's declared capabilities. For WASM blocks, these are
    /// enforced after intersection with operator config. For native blocks,
    /// they are documentation only.
    pub fn capabilities(mut self, caps: crate::BlockCapabilities) -> Self {
        self.capabilities = Some(caps);
        self
    }

    pub fn role(mut self, role: SkillRole) -> Self {
        self.role = Some(role);
        self
    }

    pub fn tool(mut self, tool: SkillTool) -> Self {
        self.tool = Some(tool);
        self
    }

    pub fn external_assets(mut self, assets: Vec<ExternalAsset>) -> Self {
        self.external_assets = assets;
        self
    }
}

/// An HTTP endpoint exposed by a block.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BlockEndpoint {
    pub method: HttpMethod,
    pub path: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub auth: AuthLevel,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_params: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_params: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deprecated: bool,
}

impl Default for BlockEndpoint {
    fn default() -> Self {
        Self {
            method: HttpMethod::Get,
            path: String::new(),
            summary: String::new(),
            auth: AuthLevel::default(),
            description: String::new(),
            input_schema: None,
            output_schema: None,
            path_params: None,
            query_params: None,
            tags: Vec::new(),
            deprecated: false,
        }
    }
}

impl BlockEndpoint {
    fn new(method: HttpMethod, path: &str) -> Self {
        Self {
            method,
            path: path.into(),
            summary: String::new(),
            auth: AuthLevel::default(),
            description: String::new(),
            input_schema: None,
            output_schema: None,
            path_params: None,
            query_params: None,
            tags: Vec::new(),
            deprecated: false,
        }
    }

    pub fn get(path: &str) -> Self {
        Self::new(HttpMethod::Get, path)
    }

    pub fn post(path: &str) -> Self {
        Self::new(HttpMethod::Post, path)
    }

    pub fn patch(path: &str) -> Self {
        Self::new(HttpMethod::Patch, path)
    }

    pub fn delete(path: &str) -> Self {
        Self::new(HttpMethod::Delete, path)
    }

    pub fn summary(mut self, summary: &str) -> Self {
        self.summary = summary.into();
        self
    }

    pub fn description(mut self, description: &str) -> Self {
        self.description = description.into();
        self
    }

    pub fn auth(mut self, auth: AuthLevel) -> Self {
        self.auth = auth;
        self
    }

    pub fn input_schema(mut self, schema: serde_json::Value) -> Self {
        self.input_schema = Some(schema);
        self
    }

    pub fn output_schema(mut self, schema: serde_json::Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    pub fn path_params_schema(mut self, schema: serde_json::Value) -> Self {
        self.path_params = Some(schema);
        self
    }

    pub fn query_params_schema(mut self, schema: serde_json::Value) -> Self {
        self.query_params = Some(schema);
        self
    }

    pub fn tags(mut self, tags: &[&str]) -> Self {
        self.tags = tags.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn deprecated(mut self) -> Self {
        self.deprecated = true;
        self
    }

    /// Returns true if any schema field is set.
    pub fn has_schema(&self) -> bool {
        self.input_schema.is_some()
            || self.output_schema.is_some()
            || self.path_params.is_some()
            || self.query_params.is_some()
    }

    #[cfg(feature = "json-schema")]
    pub fn input<T: schemars::JsonSchema>(mut self) -> Self {
        let schema = schemars::schema_for!(T);
        self.input_schema = Some(serde_json::to_value(schema).unwrap_or(serde_json::Value::Null));
        self
    }

    #[cfg(feature = "json-schema")]
    pub fn output<T: schemars::JsonSchema>(mut self) -> Self {
        let schema = schemars::schema_for!(T);
        self.output_schema = Some(serde_json::to_value(schema).unwrap_or(serde_json::Value::Null));
        self
    }

    #[cfg(feature = "json-schema")]
    pub fn path_params<T: schemars::JsonSchema>(mut self) -> Self {
        let schema = schemars::schema_for!(T);
        self.path_params = Some(serde_json::to_value(schema).unwrap_or(serde_json::Value::Null));
        self
    }

    #[cfg(feature = "json-schema")]
    pub fn query_params<T: schemars::JsonSchema>(mut self) -> Self {
        let schema = schemars::schema_for!(T);
        self.query_params = Some(serde_json::to_value(schema).unwrap_or(serde_json::Value::Null));
        self
    }
}

/// Input type for config variable UI rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum InputType {
    #[default]
    Text,
    Toggle,
    Password,
    Color,
    Url,
}

/// A configuration variable declared by a block.
///
/// This is the single source of truth for config variable metadata.
/// Validation rules are derived from naming conventions:
/// - Sensitive (masked in API): `input_type == Password`
/// - Can't be emptied: `input_type == Password`
/// - Can't be deleted: key starts with `SOLOBASE_SHARED__`
/// - URL validated on write: `input_type == Url`
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConfigVar {
    /// Full config key (e.g., `SUPPERS_AI__AUTH__JWT_SECRET`).
    pub key: String,
    /// Display label for the admin UI (e.g., "JWT Secret").
    #[serde(default)]
    pub name: String,
    /// What this variable controls.
    #[serde(default)]
    pub description: String,
    /// Default value if not set.
    #[serde(default)]
    pub default: String,
    /// Input type for UI rendering and validation.
    #[serde(default)]
    pub input_type: InputType,
    /// Optional warning shown in the admin UI (e.g., "Changing this invalidates sessions").
    #[serde(default)]
    pub warning: String,
    /// If true, a random value is auto-generated when the variable doesn't exist.
    /// Used for secrets like JWT signing keys and webhook HMAC secrets.
    #[serde(default)]
    pub auto_generate: bool,
    /// If true, this variable is admin-configurable but not required for
    /// block startup. Blocks mark vars `.optional()` when they degrade
    /// gracefully without them (e.g., OAuth credentials — block boots
    /// without them, just disables that provider). The required-config
    /// startup validator skips optional vars.
    #[serde(default)]
    pub optional: bool,
}

impl ConfigVar {
    /// Create a config var with key, description, and default value.
    /// Use builder methods for additional metadata (name, input_type, warning).
    pub fn new(key: &str, description: &str, default: &str) -> Self {
        Self {
            key: key.into(),
            name: String::new(),
            description: description.into(),
            default: default.into(),
            input_type: InputType::Text,
            warning: String::new(),
            auto_generate: false,
            optional: false,
        }
    }

    pub fn name(mut self, name: &str) -> Self {
        self.name = name.into();
        self
    }

    pub fn description(mut self, description: &str) -> Self {
        self.description = description.into();
        self
    }

    pub fn default_value(mut self, default: &str) -> Self {
        self.default = default.into();
        self
    }

    pub fn input_type(mut self, input_type: InputType) -> Self {
        self.input_type = input_type;
        self
    }

    pub fn warning(mut self, warning: &str) -> Self {
        self.warning = warning.into();
        self
    }

    /// Mark this variable as auto-generated if not provided.
    /// On first startup, a random secret is generated and stored.
    pub fn auto_generate(mut self) -> Self {
        self.auto_generate = true;
        self
    }

    /// Mark this variable as optional. See the `optional` field doc.
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// Whether this variable is sensitive (should be masked in API responses).
    pub fn is_sensitive(&self) -> bool {
        self.input_type == InputType::Password
    }

    /// Whether this variable needs URL validation on write.
    pub fn is_url(&self) -> bool {
        self.input_type == InputType::Url
    }

    /// Whether this variable can be deleted by an admin.
    /// Shared system vars cannot be deleted.
    pub fn is_deletable(&self) -> bool {
        !self.key.starts_with("SOLOBASE_SHARED__")
    }

    /// Whether this variable can be set to an empty value.
    /// Sensitive vars (passwords/secrets) cannot be emptied.
    pub fn can_be_empty(&self) -> bool {
        !self.is_sensitive()
    }
}

/// Backward-compatible alias for `ConfigVar`.
pub type BlockConfigKey = ConfigVar;

// ---------------------------------------------------------------------------
// Interface specs
// ---------------------------------------------------------------------------

/// Specification for a block interface — the contract that blocks with
/// this interface must fulfil.
///
/// Describes what the interface does, what actions it handles, and the
/// expected message/response shapes per action.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InterfaceSpec {
    /// Interface identifier, e.g. `"middleware@v1"`.
    pub name: String,
    /// Human-readable description of what blocks with this interface do.
    pub description: String,
    /// Per-action specifications. Key is the action name (e.g. `"retrieve"`,
    /// `"query"`). An empty map means the interface is action-agnostic
    /// (e.g. middleware that passes any message through).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub actions: HashMap<String, ActionSpec>,
}

/// Specification for a single action within an interface.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActionSpec {
    /// What this action does.
    pub description: String,
    /// JSON Schema describing the expected message `data` for this action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_schema: Option<serde_json::Value>,
    /// JSON Schema describing the response `data` for this action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<serde_json::Value>,
}

/// A database collection (table) declared by a block.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CollectionSchema {
    pub name: String,
    pub fields: Vec<FieldSchema>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<IndexSchema>,
}

/// A field (column) in a collection.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FieldSchema {
    pub name: String,
    pub field_type: String,
    #[serde(default)]
    pub unique: bool,
    #[serde(default)]
    pub optional: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default_value: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reference: String,
}

/// An index on a collection.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IndexSchema {
    pub fields: Vec<String>,
    #[serde(default)]
    pub unique: bool,
}

// ---------------------------------------------------------------------------
// Schema builder helpers
// ---------------------------------------------------------------------------

impl CollectionSchema {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            fields: Vec::new(),
            indexes: Vec::new(),
        }
    }

    pub fn field(mut self, name: &str, field_type: &str) -> Self {
        self.fields.push(FieldSchema::new(name, field_type));
        self
    }

    pub fn field_unique(mut self, name: &str, field_type: &str) -> Self {
        self.fields
            .push(FieldSchema::new(name, field_type).set_unique());
        self
    }

    pub fn field_optional(mut self, name: &str, field_type: &str) -> Self {
        self.fields
            .push(FieldSchema::new(name, field_type).set_optional());
        self
    }

    pub fn field_default(mut self, name: &str, field_type: &str, default: &str) -> Self {
        self.fields
            .push(FieldSchema::new(name, field_type).set_default(default));
        self
    }

    pub fn field_ref(mut self, name: &str, field_type: &str, reference: &str) -> Self {
        self.fields
            .push(FieldSchema::new(name, field_type).set_ref(reference));
        self
    }

    pub fn index(mut self, fields: &[&str]) -> Self {
        self.indexes.push(IndexSchema {
            fields: fields.iter().map(|s| s.to_string()).collect(),
            unique: false,
        });
        self
    }

    pub fn unique_index(mut self, fields: &[&str]) -> Self {
        self.indexes.push(IndexSchema {
            fields: fields.iter().map(|s| s.to_string()).collect(),
            unique: true,
        });
        self
    }
}

impl FieldSchema {
    pub fn new(name: &str, field_type: &str) -> Self {
        Self {
            name: name.to_string(),
            field_type: field_type.to_string(),
            unique: false,
            optional: false,
            default_value: String::new(),
            reference: String::new(),
        }
    }

    pub fn set_unique(mut self) -> Self {
        self.unique = true;
        self
    }
    pub fn set_optional(mut self) -> Self {
        self.optional = true;
        self
    }
    pub fn set_default(mut self, val: &str) -> Self {
        self.default_value = val.to_string();
        self
    }
    pub fn set_ref(mut self, reference: &str) -> Self {
        self.reference = reference.to_string();
        self
    }
}

/// A UI route declared by a block for SSR page serving.
///
/// Blocks declare their page routes via `Block::ui_routes()`. The router
/// automatically prefixes each path with `/b/{block_short_name}`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UiRoute {
    /// Relative path (e.g., "/login", "/dashboard", "/").
    pub path: String,
    /// Required roles to access this route.
    /// Empty = public, `["*"]` = any authenticated user, `["admin"]` = admin only.
    pub roles: Vec<String>,
}

impl UiRoute {
    pub fn new(path: &str, roles: &[&str]) -> Self {
        Self {
            path: path.to_string(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
        }
    }

    /// Public route — no authentication required.
    pub fn public(path: &str) -> Self {
        Self::new(path, &[])
    }

    /// Authenticated route — any logged-in user.
    pub fn authenticated(path: &str) -> Self {
        Self::new(path, &["*"])
    }

    /// Admin-only route.
    pub fn admin(path: &str) -> Self {
        Self::new(path, &["admin"])
    }
}

/// HTTP-level request action mapped from method to WAFER semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestAction {
    Retrieve,
    Create,
    Update,
    Delete,
    Execute,
}

impl RequestAction {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Retrieve => "retrieve",
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Execute => "execute",
        }
    }
}

// ---------------------------------------------------------------------------
// ErrorCode backward-compatible constants
// ---------------------------------------------------------------------------

impl crate::ErrorCode {
    pub const OK: Self = Self::Ok;
    pub const CANCELLED: Self = Self::Cancelled;
    pub const UNKNOWN: Self = Self::Unknown;
    pub const INVALID_ARGUMENT: Self = Self::InvalidArgument;
    pub const DEADLINE_EXCEEDED: Self = Self::DeadlineExceeded;
    pub const NOT_FOUND: Self = Self::NotFound;
    pub const ALREADY_EXISTS: Self = Self::AlreadyExists;
    pub const PERMISSION_DENIED: Self = Self::PermissionDenied;
    pub const RESOURCE_EXHAUSTED: Self = Self::ResourceExhausted;
    pub const FAILED_PRECONDITION: Self = Self::FailedPrecondition;
    pub const ABORTED: Self = Self::Aborted;
    pub const OUT_OF_RANGE: Self = Self::OutOfRange;
    pub const UNIMPLEMENTED: Self = Self::Unimplemented;
    pub const INTERNAL: Self = Self::Internal;
    pub const UNAVAILABLE: Self = Self::Unavailable;
    pub const DATA_LOSS: Self = Self::DataLoss;
    pub const UNAUTHENTICATED: Self = Self::Unauthenticated;
}

impl From<&str> for crate::ErrorCode {
    fn from(s: &str) -> Self {
        match s {
            "ok" => Self::Ok,
            "cancelled" => Self::Cancelled,
            "unknown" => Self::Unknown,
            "invalid_argument" | "invalid-argument" | "bad_request" => Self::InvalidArgument,
            "deadline_exceeded" | "deadline-exceeded" => Self::DeadlineExceeded,
            "not_found" | "not-found" => Self::NotFound,
            "already_exists" | "already-exists" => Self::AlreadyExists,
            "permission_denied" | "permission-denied" => Self::PermissionDenied,
            "resource_exhausted" | "resource-exhausted" => Self::ResourceExhausted,
            "failed_precondition" | "failed-precondition" => Self::FailedPrecondition,
            "aborted" => Self::Aborted,
            "out_of_range" | "out-of-range" => Self::OutOfRange,
            "unimplemented" | "not_implemented" => Self::Unimplemented,
            "internal" => Self::Internal,
            "unavailable" => Self::Unavailable,
            "data_loss" | "data-loss" => Self::DataLoss,
            "unauthenticated" => Self::Unauthenticated,
            _ => Self::Unknown,
        }
    }
}

// ---------------------------------------------------------------------------
// WaferError helpers
// ---------------------------------------------------------------------------

impl crate::WaferError {
    /// Create a new error with the given code and message (empty meta).
    pub fn new(code: impl Into<crate::ErrorCode>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            meta: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Message helpers
// ---------------------------------------------------------------------------

impl crate::Message {
    /// Create a new message with the given kind (empty meta).
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            meta: Vec::new(),
        }
    }

    /// Get a meta value by key, returning `""` if not found.
    pub fn get_meta(&self, key: &str) -> &str {
        self.meta
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.value.as_str())
            .unwrap_or("")
    }

    /// Set a meta value, updating an existing entry or appending a new one.
    pub fn set_meta(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        if let Some(entry) = self.meta.iter_mut().find(|e| e.key == key) {
            entry.value = value;
        } else {
            self.meta.push(crate::MetaEntry { key, value });
        }
    }

    /// Shortcut for `get_meta(META_REQ_ACTION)`.
    pub fn action(&self) -> &str {
        self.get_meta(crate::meta::META_REQ_ACTION)
    }

    /// Shortcut for `get_meta(META_REQ_RESOURCE)`.
    pub fn path(&self) -> &str {
        self.get_meta(crate::meta::META_REQ_RESOURCE)
    }

    /// Shortcut for the authenticated user ID.
    pub fn user_id(&self) -> &str {
        self.get_meta(crate::meta::META_AUTH_USER_ID)
    }

    /// Shortcut for `get_meta("req.client.ip")`.
    pub fn remote_addr(&self) -> &str {
        self.get_meta(crate::meta::META_REQ_CLIENT_IP)
    }

    /// Get a cookie value by name from the Cookie header.
    pub fn cookie(&self, name: &str) -> &str {
        let cookies = self.header("cookie");
        if cookies.is_empty() {
            return "";
        }
        for part in cookies.split(';') {
            let part = part.trim();
            if let Some(eq) = part.find('=') {
                if &part[..eq] == name {
                    return &part[eq + 1..];
                }
            }
        }
        ""
    }

    /// Get an HTTP header value (case-insensitive).
    pub fn header(&self, name: &str) -> &str {
        let key = format!("http.header.{name}");
        let val = self.get_meta(&key);
        if !val.is_empty() {
            return val;
        }
        let key_lower = format!("http.header.{}", name.to_lowercase());
        self.get_meta(&key_lower)
    }

    /// Get a URL path variable by name (from `req.param.{name}`).
    pub fn var(&self, name: &str) -> &str {
        let key = format!("{}{}", crate::meta::META_REQ_PARAM_PREFIX, name);
        self.get_meta(&key)
    }

    /// Get a query parameter by name (from `req.query.{name}`).
    pub fn query(&self, name: &str) -> &str {
        let key = format!("{}{}", crate::meta::META_REQ_QUERY_PREFIX, name);
        self.get_meta(&key)
    }

    /// Collect all query parameters into a HashMap.
    pub fn query_params(&self) -> HashMap<String, String> {
        let prefix = crate::meta::META_REQ_QUERY_PREFIX;
        self.meta
            .iter()
            .filter(|e| e.key.starts_with(prefix))
            .map(|e| (e.key[prefix.len()..].to_string(), e.value.clone()))
            .collect()
    }

    /// Parse pagination query parameters (page, page_size, offset).
    pub fn pagination_params(&self, default_page_size: usize) -> (usize, usize, usize) {
        let page: usize = self.query("page").parse().unwrap_or(1).max(1);
        let page_size: usize = self
            .query("page_size")
            .parse()
            .unwrap_or(default_page_size)
            .min(100);
        let offset = (page - 1) * page_size;
        (page, page_size, offset)
    }
}

// ---------------------------------------------------------------------------
// MetaAccess trait for Vec<MetaEntry>
// ---------------------------------------------------------------------------

/// Convenience trait giving `HashMap`-like access to `Vec<MetaEntry>`.
pub trait MetaAccess {
    fn get(&self, key: &str) -> Option<&str>;
    fn set(&mut self, key: String, value: String);
    fn contains_key(&self, key: &str) -> bool;
}

impl MetaAccess for Vec<crate::MetaEntry> {
    fn get(&self, key: &str) -> Option<&str> {
        self.iter().find(|e| e.key == key).map(|e| e.value.as_str())
    }

    fn set(&mut self, key: String, value: String) {
        if let Some(entry) = self.iter_mut().find(|e| e.key == key) {
            entry.value = value;
        } else {
            self.push(crate::MetaEntry { key, value });
        }
    }

    fn contains_key(&self, key: &str) -> bool {
        self.iter().any(|e| e.key == key)
    }
}

impl MetaAccess for [crate::MetaEntry] {
    fn get(&self, key: &str) -> Option<&str> {
        self.iter().find(|e| e.key == key).map(|e| e.value.as_str())
    }

    fn set(&mut self, _key: String, _value: String) {
        panic!("cannot insert into a slice; use Vec<MetaEntry> instead");
    }

    fn contains_key(&self, key: &str) -> bool {
        self.iter().any(|e| e.key == key)
    }
}

/// Convert a `HashMap<String, String>` into `Vec<MetaEntry>`.
pub fn hashmap_to_meta(map: HashMap<String, String>) -> Vec<crate::MetaEntry> {
    map.into_iter()
        .map(|(k, v)| crate::MetaEntry { key: k, value: v })
        .collect()
}

/// Convert `Vec<MetaEntry>` into `HashMap<String, String>`.
pub fn meta_to_hashmap(meta: &[crate::MetaEntry]) -> HashMap<String, String> {
    meta.iter()
        .map(|e| (e.key.clone(), e.value.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// Skill metadata (consumed by gizza-ai/agent and any future agent block)
// ---------------------------------------------------------------------------

/// Marker for blocks that should be enumerated as tools by an agent block.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillRole {
    /// Block is callable as a tool by an agent block.
    Skill,
}

/// JSON-Schema-shaped tool descriptor for OpenAI-compatible function calling.
/// Mirrors the shape consumed by WebLLM and remote LLM providers.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillTool {
    pub description: String,
    /// Free-form JSON Schema describing the tool's input arguments.
    pub parameters: serde_json::Value,
}

/// Declarative pointer to a heavy external WASM/JS asset that the host
/// loads lazily on first use (e.g. ffmpeg-core.wasm from a CDN).
///
/// `loader` is a controlled vocabulary on the host side. Known values:
/// - `"ffmpeg.wasm"` — initialised via `@ffmpeg/ffmpeg`'s `createFFmpeg`.
///
/// New loader values require a host update; new assets that target an
/// existing loader do not.
///
/// `timeout_ms` lets the block override the host's default load timeout
/// (currently 120s in solobase-browser's `bridge.js`). `None` keeps the
/// host default. Useful for assets whose CDN download legitimately takes
/// longer than the default on slow links (e.g. ffmpeg-core ~31 MB).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExternalAsset {
    pub id: String,
    pub loader: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    /// Optional per-asset load timeout in milliseconds. When `None`, the
    /// host applies its default. `skip_serializing_if = "Option::is_none"`
    /// keeps the JSON wire format byte-identical for callers that don't
    /// set the field, so existing serialized payloads remain unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u32>,
}

// ---------------------------------------------------------------------------
// InstanceMode helpers
// ---------------------------------------------------------------------------

impl std::fmt::Display for crate::InstanceMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            crate::InstanceMode::PerNode => write!(f, "per-node"),
            crate::InstanceMode::Singleton => write!(f, "singleton"),
            crate::InstanceMode::PerFlow => write!(f, "per-flow"),
            crate::InstanceMode::PerExecution => write!(f, "per-execution"),
        }
    }
}

impl crate::InstanceMode {
    /// Parse an instance mode from a string (e.g. from flow config).
    pub fn parse(s: &str) -> Self {
        match s {
            "singleton" => Self::Singleton,
            "per-flow" => Self::PerFlow,
            "per-execution" => Self::PerExecution,
            _ => Self::PerNode,
        }
    }
}

#[cfg(test)]
mod block_info_tests {
    use super::*;

    #[test]
    fn block_info_capabilities_default_none() {
        let info = BlockInfo::new("org/b", "0.1.0", "iface@v1", "summary");
        assert!(info.capabilities.is_none());
    }

    #[test]
    fn block_info_capabilities_builder_sets_some() {
        let caps = crate::BlockCapabilities {
            crypto: true,
            ..Default::default()
        };
        let info = BlockInfo::new("org/b", "0.1.0", "iface@v1", "summary").capabilities(caps);
        assert!(info.capabilities.is_some());
        assert!(info.capabilities.as_ref().unwrap().crypto);
    }

    #[test]
    fn block_info_capabilities_roundtrip_json() {
        let mut caps = crate::BlockCapabilities {
            crypto: true,
            ..Default::default()
        };
        caps.headers.writable = vec!["set-cookie".into()];
        let info = BlockInfo::new("org/b", "0.1.0", "iface@v1", "summary").capabilities(caps);
        let j = serde_json::to_string(&info).unwrap();
        let back: BlockInfo = serde_json::from_str(&j).unwrap();
        let caps_back = back.capabilities.expect("caps present");
        assert!(caps_back.crypto);
        assert_eq!(caps_back.headers.writable, vec!["set-cookie".to_string()]);
    }

    #[test]
    fn block_info_without_capabilities_serializes_without_key() {
        let info = BlockInfo::new("org/b", "0.1.0", "iface@v1", "summary");
        let j = serde_json::to_string(&info).unwrap();
        assert!(
            !j.contains("\"capabilities\""),
            "json should omit the field when None: {j}"
        );
    }
}

#[cfg(test)]
mod block_endpoint_tests {
    use super::*;

    #[test]
    fn builder_basic() {
        let ep = BlockEndpoint::post("/b/auth/api/login")
            .summary("Authenticate user")
            .description("Login with email/password")
            .auth(AuthLevel::Public)
            .tags(&["auth"]);
        assert_eq!(ep.method, HttpMethod::Post);
        assert_eq!(ep.path, "/b/auth/api/login");
        assert_eq!(ep.summary, "Authenticate user");
        assert_eq!(ep.description, "Login with email/password");
        assert_eq!(ep.auth, AuthLevel::Public);
        assert_eq!(ep.tags, vec!["auth".to_string()]);
        assert!(ep.input_schema.is_none());
        assert!(!ep.deprecated);
    }

    #[test]
    fn builder_with_manual_schemas() {
        let ep = BlockEndpoint::get("/b/files/api/objects")
            .summary("List objects")
            .auth(AuthLevel::Authenticated)
            .input_schema(serde_json::json!({"type": "object", "properties": {"prefix": {"type": "string"}}}))
            .output_schema(serde_json::json!({"type": "array", "items": {"type": "object"}}))
            .path_params_schema(serde_json::json!({"type": "object", "properties": {"bucket": {"type": "string"}}, "required": ["bucket"]}))
            .query_params_schema(serde_json::json!({"type": "object", "properties": {"limit": {"type": "integer"}}}));
        assert!(ep.input_schema.is_some());
        assert!(ep.output_schema.is_some());
        assert!(ep.path_params.is_some());
        assert!(ep.query_params.is_some());
    }

    #[test]
    fn builder_defaults() {
        let ep = BlockEndpoint::get("/health").summary("Health check");
        assert_eq!(ep.auth, AuthLevel::Public);
        assert!(ep.description.is_empty());
        assert!(ep.tags.is_empty());
        assert!(!ep.deprecated);
    }

    #[test]
    fn has_schema_false_when_no_schemas() {
        let ep = BlockEndpoint::get("/health").summary("Health check");
        assert!(!ep.has_schema());
    }

    #[test]
    fn has_schema_true_with_output() {
        let ep = BlockEndpoint::get("/health")
            .summary("Health check")
            .output_schema(serde_json::json!({"type": "object"}));
        assert!(ep.has_schema());
    }
}
