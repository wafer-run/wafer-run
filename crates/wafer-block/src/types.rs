//! Runtime-only types and convenience impls on core types.
//!
//! Core types (`Message`, `MetaEntry`, `ErrorCode`, `WaferError`) are defined
//! in `core_types`. This module adds:
//!
//! - Runtime-only types (`BlockInfo`, `AdminUIInfo`, `BlockRuntime`, `RequestAction`)
//! - Ergonomic constructors and helpers on core types
//! - Backward-compatible `SCREAMING_CASE` constants on `ErrorCode`

use std::collections::HashMap;

/// Reserved prefix for cross-block shared config variables.
///
/// Keys with this prefix are special-cased throughout the runtime:
/// - [`ConfigVar::is_deletable`] returns `false` (admins can't delete them).
/// - WRAP (`wafer_block::wrap::check_access`) treats reads as readable by any
///   attributable caller but writes as admin-only.
/// - [`BlockInfo::validate`] rejects blocks that try to *declare* a key under
///   this prefix (the prefix is owned by the platform, not any one block).
///
/// Same literal everywhere — env vars, D1, and the config API all use this
/// exact string with no translation.
pub const SOLOBASE_SHARED_PREFIX: &str = "SOLOBASE_SHARED__";

// ---------------------------------------------------------------------------
// Runtime-only types (not part of WIT)
// ---------------------------------------------------------------------------

/// HTTP method for block endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HttpMethod {
    /// HTTP `GET`.
    #[serde(rename = "GET")]
    Get,
    /// HTTP `POST`.
    #[serde(rename = "POST")]
    Post,
    /// HTTP `PATCH`.
    #[serde(rename = "PATCH")]
    Patch,
    /// HTTP `DELETE`.
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
    /// No authentication required.
    #[default]
    Public,
    /// Any logged-in user is allowed.
    Authenticated,
    /// Admin role required.
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
    /// Block compiled into the host binary as native code.
    #[default]
    Native,
    /// Block loaded as a sandboxed WASM module.
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
    /// Parse a `ResourceType` from its lowercase string form. Returns `None`
    /// for unrecognized values.
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

/// Validation failures raised by [`BlockInfo::validate`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlockInfoError {
    /// A block declared a `config_keys` / `flow_config` entry whose name
    /// starts with [`SOLOBASE_SHARED_PREFIX`]. That prefix is platform-owned
    /// (shared variables are seeded and write-gated centrally), so a block
    /// declaring one would create a key it cannot legitimately own.
    #[error(
        "block '{block}' declares reserved config key '{key}': keys starting with '{SOLOBASE_SHARED_PREFIX}' are platform-owned and cannot be declared by a block"
    )]
    ReservedConfigKey {
        /// Name of the block that declared the offending key.
        block: String,
        /// The reserved config key the block tried to declare.
        key: String,
    },
}

/// Block metadata — identity, schema declarations, and admin UI metadata.
///
/// Only `name`, `version`, `interface`, and `summary` are required.
/// All other fields have sensible defaults via `Default`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BlockInfo {
    // -- Core identity --
    /// Block name in the canonical `{org}/{block}` form.
    pub name: String,
    /// Semantic version string for the block implementation.
    pub version: String,
    /// Interface identifier (e.g. `"middleware@v1"`) — see [`InterfaceSpec`].
    pub interface: String,
    /// One-line human-readable summary of what the block does.
    pub summary: String,
    /// How many instances are created and when (default: `PerNode`).
    #[serde(default = "default_instance_mode")]
    pub instance_mode: crate::InstanceMode,
    /// Names of other blocks this block depends on. Used by the runtime to
    /// validate the registry and (eventually) order initialization.
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

    /// Validate declared config keys against platform-reserved prefixes.
    ///
    /// A block may not *declare* a `config_keys` or `flow_config` entry whose
    /// name starts with [`SOLOBASE_SHARED_PREFIX`]. That prefix is owned by the
    /// platform — shared variables are seeded and write-gated centrally (see
    /// [`ConfigVar::is_deletable`] and `wafer_block::wrap::check_access`), so a
    /// block declaring one would create a key it cannot legitimately own.
    ///
    /// Called at block registration time by the runtime; returns the first
    /// offending key as a typed [`BlockInfoError`] so boot fails loudly and
    /// callers can match on the failure rather than parse a string.
    pub fn validate(&self) -> Result<(), BlockInfoError> {
        for var in self.config_keys.iter().chain(self.flow_config.iter()) {
            if var.key.starts_with(SOLOBASE_SHARED_PREFIX) {
                return Err(BlockInfoError::ReservedConfigKey {
                    block: self.name.clone(),
                    key: var.key.clone(),
                });
            }
        }
        Ok(())
    }

    /// Set the [`crate::InstanceMode`] (default: `PerNode`).
    pub fn instance_mode(mut self, mode: crate::InstanceMode) -> Self {
        self.instance_mode = mode;
        self
    }

    /// Set the list of dependency block names.
    pub fn requires(mut self, requires: Vec<String>) -> Self {
        self.requires = requires;
        self
    }

    /// Set the database collections this block declares.
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

    /// Set the process env-var-style config keys this block declares.
    pub fn config_keys(mut self, config_keys: Vec<ConfigVar>) -> Self {
        self.config_keys = config_keys;
        self
    }

    /// Set the WRAP resource grants this block issues to other blocks.
    pub fn grants(mut self, grants: Vec<ResourceGrant>) -> Self {
        self.grants = grants;
        self
    }

    /// Set the admin-UI category for this block.
    pub fn category(mut self, category: BlockCategory) -> Self {
        self.category = category;
        self
    }

    /// Mark this block as singleton infrastructure — shorthand for
    /// `.instance_mode(InstanceMode::Singleton).category(BlockCategory::Infrastructure)`.
    ///
    /// Used by the runtime's infra blocks (CORS, security-headers, router,
    /// rate-limit, monitoring, …) that hold one shared instance per node and
    /// are surfaced under the Infrastructure admin category.
    pub fn infrastructure(self) -> Self {
        self.instance_mode(crate::InstanceMode::Singleton)
            .category(BlockCategory::Infrastructure)
    }

    /// Set the runtime kind (native vs WASM).
    pub fn runtime(mut self, runtime: BlockRuntime) -> Self {
        self.runtime = runtime;
        self
    }

    /// Mark this block as admin-disable-able.
    pub fn can_disable(mut self, can_disable: bool) -> Self {
        self.can_disable = can_disable;
        self
    }

    /// Set whether the block is enabled on first run.
    pub fn default_enabled(mut self, default_enabled: bool) -> Self {
        self.default_enabled = default_enabled;
        self
    }

    /// Set the longer description shown in the admin UI.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Set the HTTP endpoints exposed by this block.
    pub fn endpoints(mut self, endpoints: Vec<BlockEndpoint>) -> Self {
        self.endpoints = endpoints;
        self
    }

    /// Set the relative URL where the block's admin UI lives.
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

    /// Mark this block as an agent-callable skill.
    pub fn role(mut self, role: SkillRole) -> Self {
        self.role = Some(role);
        self
    }

    /// Attach the OpenAI-compatible tool descriptor used by agent blocks.
    pub fn tool(mut self, tool: SkillTool) -> Self {
        self.tool = Some(tool);
        self
    }

    /// Declare heavy external WASM/JS assets the host must lazily fetch.
    pub fn external_assets(mut self, assets: Vec<ExternalAsset>) -> Self {
        self.external_assets = assets;
        self
    }
}

/// An HTTP endpoint exposed by a block.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BlockEndpoint {
    /// HTTP method this endpoint responds to.
    pub method: HttpMethod,
    /// Absolute URL path (typically `/b/{block}/...`).
    pub path: String,
    /// Short summary shown in the admin/OpenAPI UI.
    #[serde(default)]
    pub summary: String,
    /// Auth level required by the router to admit a request.
    #[serde(default)]
    pub auth: AuthLevel,
    /// Longer description for OpenAPI / docs.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// JSON Schema describing the request body, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    /// JSON Schema describing the response body, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    /// JSON Schema describing URL path parameters, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_params: Option<serde_json::Value>,
    /// JSON Schema describing query parameters, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_params: Option<serde_json::Value>,
    /// Free-form tags for grouping endpoints in OpenAPI.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Whether the endpoint is marked deprecated.
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

    /// Create a `GET` endpoint at `path`.
    pub fn get(path: &str) -> Self {
        Self::new(HttpMethod::Get, path)
    }

    /// Create a `POST` endpoint at `path`.
    pub fn post(path: &str) -> Self {
        Self::new(HttpMethod::Post, path)
    }

    /// Create a `PATCH` endpoint at `path`.
    pub fn patch(path: &str) -> Self {
        Self::new(HttpMethod::Patch, path)
    }

    /// Create a `DELETE` endpoint at `path`.
    pub fn delete(path: &str) -> Self {
        Self::new(HttpMethod::Delete, path)
    }

    /// Set the short summary text.
    pub fn summary(mut self, summary: &str) -> Self {
        self.summary = summary.into();
        self
    }

    /// Set the longer description text.
    pub fn description(mut self, description: &str) -> Self {
        self.description = description.into();
        self
    }

    /// Set the required [`AuthLevel`].
    pub fn auth(mut self, auth: AuthLevel) -> Self {
        self.auth = auth;
        self
    }

    /// Attach a manually-specified JSON Schema for the request body.
    pub fn input_schema(mut self, schema: serde_json::Value) -> Self {
        self.input_schema = Some(schema);
        self
    }

    /// Attach a manually-specified JSON Schema for the response body.
    pub fn output_schema(mut self, schema: serde_json::Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    /// Attach a manually-specified JSON Schema for URL path parameters.
    pub fn path_params_schema(mut self, schema: serde_json::Value) -> Self {
        self.path_params = Some(schema);
        self
    }

    /// Attach a manually-specified JSON Schema for query parameters.
    pub fn query_params_schema(mut self, schema: serde_json::Value) -> Self {
        self.query_params = Some(schema);
        self
    }

    /// Set the OpenAPI tag list.
    pub fn tags(mut self, tags: &[&str]) -> Self {
        self.tags = tags.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Mark the endpoint as deprecated.
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

    /// Derive the request-body JSON Schema from `T` via `schemars`.
    #[cfg(feature = "json-schema")]
    pub fn input<T: schemars::JsonSchema>(mut self) -> Self {
        let schema = schemars::schema_for!(T);
        self.input_schema = Some(serde_json::to_value(schema).unwrap_or(serde_json::Value::Null));
        self
    }

    /// Derive the response-body JSON Schema from `T` via `schemars`.
    #[cfg(feature = "json-schema")]
    pub fn output<T: schemars::JsonSchema>(mut self) -> Self {
        let schema = schemars::schema_for!(T);
        self.output_schema = Some(serde_json::to_value(schema).unwrap_or(serde_json::Value::Null));
        self
    }

    /// Derive the path-params JSON Schema from `T` via `schemars`.
    #[cfg(feature = "json-schema")]
    pub fn path_params<T: schemars::JsonSchema>(mut self) -> Self {
        let schema = schemars::schema_for!(T);
        self.path_params = Some(serde_json::to_value(schema).unwrap_or(serde_json::Value::Null));
        self
    }

    /// Derive the query-params JSON Schema from `T` via `schemars`.
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
    /// Plain text input.
    #[default]
    Text,
    /// Boolean on/off toggle.
    Toggle,
    /// Password field (masked, treated as sensitive).
    Password,
    /// Color picker.
    Color,
    /// URL field (validated on write).
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

    /// Set the display label for the admin UI.
    pub fn name(mut self, name: &str) -> Self {
        self.name = name.into();
        self
    }

    /// Set the human-readable description.
    pub fn description(mut self, description: &str) -> Self {
        self.description = description.into();
        self
    }

    /// Set the default value used when the variable is not configured.
    pub fn default_value(mut self, default: &str) -> Self {
        self.default = default.into();
        self
    }

    /// Set the UI [`InputType`] (drives masking, validation, widget).
    pub fn input_type(mut self, input_type: InputType) -> Self {
        self.input_type = input_type;
        self
    }

    /// Attach a warning string shown in the admin UI on edit.
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
        !self.key.starts_with(SOLOBASE_SHARED_PREFIX)
    }

    /// Whether this variable can be set to an empty value.
    /// Sensitive vars (passwords/secrets) cannot be emptied.
    pub fn can_be_empty(&self) -> bool {
        !self.is_sensitive()
    }
}

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
    /// Collection (table) name, typically `{org}__{block}__{name}`.
    pub name: String,
    /// Field (column) definitions.
    pub fields: Vec<FieldSchema>,
    /// Indexes to be ensured on the collection.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<IndexSchema>,
}

/// A field (column) in a collection.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FieldSchema {
    /// Column name.
    pub name: String,
    /// Backend-portable type name (e.g. `"text"`, `"integer"`, `"json"`).
    pub field_type: String,
    /// Whether values in this column must be unique.
    #[serde(default)]
    pub unique: bool,
    /// Whether the column is nullable.
    #[serde(default)]
    pub optional: bool,
    /// Default value expression (empty = no default).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default_value: String,
    /// Optional foreign-key reference, formatted as `"table.column"`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reference: String,
}

/// An index on a collection.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IndexSchema {
    /// Column names included in the index, in order.
    pub fields: Vec<String>,
    /// Whether the index enforces a uniqueness constraint.
    #[serde(default)]
    pub unique: bool,
}

// ---------------------------------------------------------------------------
// Schema builder helpers
// ---------------------------------------------------------------------------

impl CollectionSchema {
    /// Start a new collection definition with the given table name.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            fields: Vec::new(),
            indexes: Vec::new(),
        }
    }

    /// Append a plain field of `field_type`.
    pub fn field(mut self, name: &str, field_type: &str) -> Self {
        self.fields.push(FieldSchema::new(name, field_type));
        self
    }

    /// Append a uniqueness-constrained field.
    pub fn field_unique(mut self, name: &str, field_type: &str) -> Self {
        self.fields
            .push(FieldSchema::new(name, field_type).set_unique());
        self
    }

    /// Append a nullable field.
    pub fn field_optional(mut self, name: &str, field_type: &str) -> Self {
        self.fields
            .push(FieldSchema::new(name, field_type).set_optional());
        self
    }

    /// Append a field with a default value.
    pub fn field_default(mut self, name: &str, field_type: &str, default: &str) -> Self {
        self.fields
            .push(FieldSchema::new(name, field_type).set_default(default));
        self
    }

    /// Append a foreign-key field referencing `reference` (`"table.column"`).
    pub fn field_ref(mut self, name: &str, field_type: &str, reference: &str) -> Self {
        self.fields
            .push(FieldSchema::new(name, field_type).set_ref(reference));
        self
    }

    /// Append a non-unique index over the given fields.
    pub fn index(mut self, fields: &[&str]) -> Self {
        self.indexes.push(IndexSchema {
            fields: fields.iter().map(|s| s.to_string()).collect(),
            unique: false,
        });
        self
    }

    /// Append a unique index over the given fields.
    pub fn unique_index(mut self, fields: &[&str]) -> Self {
        self.indexes.push(IndexSchema {
            fields: fields.iter().map(|s| s.to_string()).collect(),
            unique: true,
        });
        self
    }
}

impl FieldSchema {
    /// Create a new field with the given name and type.
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

    /// Mark the field as unique.
    pub fn set_unique(mut self) -> Self {
        self.unique = true;
        self
    }
    /// Mark the field as nullable.
    pub fn set_optional(mut self) -> Self {
        self.optional = true;
        self
    }
    /// Set the field's default value expression.
    pub fn set_default(mut self, val: &str) -> Self {
        self.default_value = val.to_string();
        self
    }
    /// Set the field's foreign-key reference (`"table.column"`).
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
    /// Build a UI route with an explicit list of required role names.
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
///
/// The associated `&str` constants ([`Self::RETRIEVE`] etc.) are the
/// single source of truth for the on-the-wire action names. Blocks that
/// emit actions (`http-listener`, `router`) or filter on them
/// (`readonly-guard`) reference these constants instead of duplicating
/// the string literals — same name, same place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestAction {
    /// Read (GET).
    Retrieve,
    /// Create (POST).
    Create,
    /// Mutate (PATCH/PUT).
    Update,
    /// Remove (DELETE).
    Delete,
    /// Custom RPC-style action (POST without CRUD semantics).
    Execute,
}

impl RequestAction {
    /// Wire constant for [`Self::Retrieve`].
    pub const RETRIEVE: &'static str = "retrieve";
    /// Wire constant for [`Self::Create`].
    pub const CREATE: &'static str = "create";
    /// Wire constant for [`Self::Update`].
    pub const UPDATE: &'static str = "update";
    /// Wire constant for [`Self::Delete`].
    pub const DELETE: &'static str = "delete";
    /// Wire constant for [`Self::Execute`].
    pub const EXECUTE: &'static str = "execute";

    /// Return the canonical lowercase string for this action.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Retrieve => Self::RETRIEVE,
            Self::Create => Self::CREATE,
            Self::Update => Self::UPDATE,
            Self::Delete => Self::DELETE,
            Self::Execute => Self::EXECUTE,
        }
    }
}

// ---------------------------------------------------------------------------
// ErrorCode backward-compatible constants
// ---------------------------------------------------------------------------

/// Backward-compatible `SCREAMING_CASE` aliases for [`crate::ErrorCode`]
/// variants. Newer code should prefer the variant names directly.
impl crate::ErrorCode {
    /// Alias for [`Self::Ok`].
    pub const OK: Self = Self::Ok;
    /// Alias for [`Self::Cancelled`].
    pub const CANCELLED: Self = Self::Cancelled;
    /// Alias for [`Self::Unknown`].
    pub const UNKNOWN: Self = Self::Unknown;
    /// Alias for [`Self::InvalidArgument`].
    pub const INVALID_ARGUMENT: Self = Self::InvalidArgument;
    /// Alias for [`Self::DeadlineExceeded`].
    pub const DEADLINE_EXCEEDED: Self = Self::DeadlineExceeded;
    /// Alias for [`Self::NotFound`].
    pub const NOT_FOUND: Self = Self::NotFound;
    /// Alias for [`Self::AlreadyExists`].
    pub const ALREADY_EXISTS: Self = Self::AlreadyExists;
    /// Alias for [`Self::PermissionDenied`].
    pub const PERMISSION_DENIED: Self = Self::PermissionDenied;
    /// Alias for [`Self::ResourceExhausted`].
    pub const RESOURCE_EXHAUSTED: Self = Self::ResourceExhausted;
    /// Alias for [`Self::FailedPrecondition`].
    pub const FAILED_PRECONDITION: Self = Self::FailedPrecondition;
    /// Alias for [`Self::Aborted`].
    pub const ABORTED: Self = Self::Aborted;
    /// Alias for [`Self::OutOfRange`].
    pub const OUT_OF_RANGE: Self = Self::OutOfRange;
    /// Alias for [`Self::Unimplemented`].
    pub const UNIMPLEMENTED: Self = Self::Unimplemented;
    /// Alias for [`Self::Internal`].
    pub const INTERNAL: Self = Self::Internal;
    /// Alias for [`Self::Unavailable`].
    pub const UNAVAILABLE: Self = Self::Unavailable;
    /// Alias for [`Self::DataLoss`].
    pub const DATA_LOSS: Self = Self::DataLoss;
    /// Alias for [`Self::Unauthenticated`].
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
            .map_or("", |entry| entry.value.as_str())
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
    ///
    /// `page` is an unbounded, externally-supplied query value, so the offset is
    /// computed with saturating arithmetic: a hostile `?page=<huge>` clamps the
    /// offset to `usize::MAX` (yielding an empty page) instead of overflowing —
    /// which would panic in debug builds and silently wrap in release.
    pub fn pagination_params(&self, default_page_size: usize) -> (usize, usize, usize) {
        let page: usize = self.query("page").parse().unwrap_or(1).max(1);
        let page_size: usize = self
            .query("page_size")
            .parse()
            .unwrap_or(default_page_size)
            .min(100);
        let offset = page.saturating_sub(1).saturating_mul(page_size);
        (page, page_size, offset)
    }
}

// ---------------------------------------------------------------------------
// Meta access traits for `Vec<MetaEntry>` / `[MetaEntry]`
// ---------------------------------------------------------------------------

/// Read-only `HashMap`-like access to a sequence of [`crate::MetaEntry`].
///
/// Implemented for both `Vec<MetaEntry>` and the `[MetaEntry]` slice, so
/// callers holding only a `&[MetaEntry]` can still look entries up. Mutation
/// lives on the separate [`MetaSet`] trait, which is implemented only for the
/// owning `Vec` — a slice can't grow, so it can never expose a setter.
pub trait MetaGet {
    /// Look up the value for `key`, returning `None` if absent.
    fn get(&self, key: &str) -> Option<&str>;
    /// Whether an entry for `key` exists.
    fn contains_key(&self, key: &str) -> bool;
}

/// Mutating `HashMap`-like access to an owning `Vec<MetaEntry>`.
///
/// Deliberately *not* implemented for `[MetaEntry]`: a slice is a fixed-size
/// view and cannot accept new entries, so insertion is only meaningful on the
/// owning `Vec`. Splitting this off from [`MetaGet`] makes that a compile-time
/// guarantee rather than a runtime panic.
pub trait MetaSet {
    /// Set `key` to `value`, replacing any existing entry with the same key.
    fn set(&mut self, key: String, value: String);
}

impl MetaGet for Vec<crate::MetaEntry> {
    fn get(&self, key: &str) -> Option<&str> {
        // Delegate to the slice impl so the lookup logic lives in one place.
        MetaGet::get(self.as_slice(), key)
    }

    fn contains_key(&self, key: &str) -> bool {
        MetaGet::contains_key(self.as_slice(), key)
    }
}

impl MetaSet for Vec<crate::MetaEntry> {
    fn set(&mut self, key: String, value: String) {
        if let Some(entry) = self.iter_mut().find(|e| e.key == key) {
            entry.value = value;
        } else {
            self.push(crate::MetaEntry { key, value });
        }
    }
}

impl MetaGet for [crate::MetaEntry] {
    fn get(&self, key: &str) -> Option<&str> {
        self.iter().find(|e| e.key == key).map(|e| e.value.as_str())
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
    /// Natural-language description shown to the LLM.
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
    /// Stable asset identifier used by the host loader (e.g. `"ffmpeg-core"`).
    pub id: String,
    /// Controlled-vocabulary loader name the host knows how to invoke.
    pub loader: String,
    /// Asset version string for cache-busting/audit.
    pub version: String,
    /// Source URL the host fetches the asset from.
    pub url: String,
    /// Expected SHA-256 (hex) of the asset bytes, verified after download.
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

    #[test]
    fn validate_accepts_block_with_no_reserved_keys() {
        let info = BlockInfo::new("org/b", "0.1.0", "iface@v1", "summary")
            .config_keys(vec![ConfigVar::new("ORG__B__SOMETHING", "desc", "")]);
        assert!(info.validate().is_ok());
    }

    #[test]
    fn validate_rejects_reserved_prefix_in_config_keys() {
        let key = format!("{SOLOBASE_SHARED_PREFIX}APP_NAME");
        let info = BlockInfo::new("org/b", "0.1.0", "iface@v1", "summary")
            .config_keys(vec![ConfigVar::new(&key, "desc", "")]);
        let err = info
            .validate()
            .expect_err("reserved prefix must be rejected");
        assert_eq!(
            err,
            BlockInfoError::ReservedConfigKey {
                block: "org/b".to_string(),
                key: key.clone(),
            }
        );
        // Display still carries the operator-facing context.
        let msg = err.to_string();
        assert!(msg.contains(SOLOBASE_SHARED_PREFIX), "message: {msg}");
        assert!(msg.contains("org/b"), "message: {msg}");
        assert!(msg.contains(&key), "message: {msg}");
    }

    #[test]
    fn validate_rejects_reserved_prefix_in_flow_config() {
        let info = BlockInfo::new("org/b", "0.1.0", "iface@v1", "summary").flow_config(vec![
            ConfigVar::new(&format!("{SOLOBASE_SHARED_PREFIX}feature_flag"), "desc", ""),
        ]);
        assert!(info.validate().is_err());
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

#[cfg(test)]
mod meta_access_tests {
    use super::*;
    use crate::MetaEntry;

    fn sample() -> Vec<MetaEntry> {
        vec![
            MetaEntry {
                key: "a".into(),
                value: "1".into(),
            },
            MetaEntry {
                key: "b".into(),
                value: "2".into(),
            },
        ]
    }

    #[test]
    fn slice_exposes_read_methods() {
        let v = sample();
        let slice: &[MetaEntry] = v.as_slice();
        assert_eq!(MetaGet::get(slice, "a"), Some("1"));
        assert_eq!(MetaGet::get(slice, "missing"), None);
        assert!(MetaGet::contains_key(slice, "b"));
        assert!(!MetaGet::contains_key(slice, "missing"));
    }

    #[test]
    fn vec_read_methods_match_slice() {
        let v = sample();
        assert_eq!(MetaGet::get(&v, "a"), Some("1"));
        assert!(MetaGet::contains_key(&v, "b"));
        assert!(!MetaGet::contains_key(&v, "missing"));
    }

    #[test]
    fn vec_set_inserts_then_replaces() {
        let mut v = sample();
        // Insert a new key.
        MetaSet::set(&mut v, "c".into(), "3".into());
        assert_eq!(MetaGet::get(&v, "c"), Some("3"));
        assert_eq!(v.len(), 3);
        // Replace an existing key in place (no growth).
        MetaSet::set(&mut v, "a".into(), "99".into());
        assert_eq!(MetaGet::get(&v, "a"), Some("99"));
        assert_eq!(v.len(), 3);
    }
}

#[cfg(test)]
mod pagination_tests {
    fn msg_with_query(name: &str, value: &str) -> crate::Message {
        let mut m = crate::Message::new("test");
        m.set_meta(
            format!("{}{}", crate::meta::META_REQ_QUERY_PREFIX, name),
            value,
        );
        m
    }

    #[test]
    fn defaults_to_page_one_when_absent() {
        let m = crate::Message::new("test");
        assert_eq!(m.pagination_params(25), (1, 25, 0));
    }

    #[test]
    fn computes_offset_from_page_and_size() {
        let m = msg_with_query("page", "3");
        assert_eq!(m.pagination_params(20), (3, 20, 40));
    }

    #[test]
    fn page_size_is_capped_at_100() {
        let mut m = msg_with_query("page", "1");
        m.set_meta(
            format!("{}page_size", crate::meta::META_REQ_QUERY_PREFIX),
            "10000",
        );
        let (_, page_size, _) = m.pagination_params(20);
        assert_eq!(page_size, 100);
    }

    #[test]
    fn huge_page_saturates_offset_instead_of_overflowing() {
        // Regression: an unbounded `?page=` query value must not overflow the
        // `(page - 1) * page_size` multiply (debug panic / release wraparound).
        let m = msg_with_query("page", &usize::MAX.to_string());
        let (page, page_size, offset) = m.pagination_params(50);
        assert_eq!(page, usize::MAX);
        assert_eq!(page_size, 50);
        assert_eq!(offset, usize::MAX);
    }
}
