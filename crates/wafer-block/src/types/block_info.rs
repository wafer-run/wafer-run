//! Block identity and admin metadata — [`BlockInfo`] and its builders.

use super::{
    config_var::{ConfigVar, WAFER_RUN_SHARED_PREFIX},
    endpoint::{AgentTool, BlockEndpoint, HttpMethod},
    grants::ResourceGrant,
    schema::CollectionSchema,
    skill::{ExternalAsset, SkillTool},
};

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
    /// starts with [`WAFER_RUN_SHARED_PREFIX`]. That prefix is platform-owned
    /// (shared variables are seeded and write-gated centrally), so a block
    /// declaring one would create a key it cannot legitimately own.
    #[error(
        "block '{block}' declares reserved config key '{key}': keys starting with '{WAFER_RUN_SHARED_PREFIX}' are platform-owned and cannot be declared by a block"
    )]
    ReservedConfigKey {
        /// Name of the block that declared the offending key.
        block: String,
        /// The reserved config key the block tried to declare.
        key: String,
    },

    /// An endpoint's [`AgentTool`] name is not a legal MCP tool name — see
    /// [`AgentTool::is_valid_name`]. Caught at registration because the
    /// alternative is silence: an MCP client rejects the name inside the
    /// consumer's per-tool `try`/`catch`, so the tool disappears with no
    /// error anywhere, and an *empty* name additionally collides with every
    /// other empty one and suppresses unrelated tools.
    #[error(
        "block '{block}' endpoint {method} {path} declares agent tool name '{name}': tool names must be 1-{max} characters of [A-Za-z0-9_-]",
        max = AgentTool::MAX_NAME_LEN
    )]
    InvalidAgentToolName {
        /// Name of the block that declared the offending tool.
        block: String,
        /// HTTP method of the endpoint carrying the tool.
        method: HttpMethod,
        /// URL path of the endpoint carrying the tool.
        path: String,
        /// The rejected tool name.
        name: String,
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
    /// Declared instance lifecycle (default: `PerNode`). **Advisory —
    /// not enforced.** Actual behavior is fixed by runtime type: native
    /// blocks are one shared instance per runtime process, WASM blocks
    /// are a fresh instance per call. See [`crate::InstanceMode`].
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
    /// OpenAI-compatible tool descriptor. `Some` marks this block as an
    /// agent-callable skill; `None` means it is not exposed as a tool.
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
            tool: None,
            external_assets: Vec::new(),
        }
    }

    /// Validate declared config keys and agent-tool names.
    ///
    /// A block may not *declare* a `config_keys` or `flow_config` entry whose
    /// name starts with [`WAFER_RUN_SHARED_PREFIX`]. That prefix is owned by the
    /// platform — shared variables are seeded and write-gated centrally (see
    /// [`ConfigVar::is_deletable`] and `wafer_block::wrap::check_access`), so a
    /// block declaring one would create a key it cannot legitimately own.
    ///
    /// An endpoint's [`AgentTool`] name must be a legal MCP tool name — see
    /// [`AgentTool::is_valid_name`] for the rule and for what goes wrong
    /// downstream when it is not.
    ///
    /// Called at block registration time by the runtime; returns the first
    /// offending declaration as a typed [`BlockInfoError`] so boot fails
    /// loudly and callers can match on the failure rather than parse a
    /// string.
    pub fn validate(&self) -> Result<(), BlockInfoError> {
        for var in self.config_keys.iter().chain(self.flow_config.iter()) {
            if var.key.starts_with(WAFER_RUN_SHARED_PREFIX) {
                return Err(BlockInfoError::ReservedConfigKey {
                    block: self.name.clone(),
                    key: var.key.clone(),
                });
            }
        }
        for ep in &self.endpoints {
            let Some(tool) = ep.agent_tool.as_ref() else {
                continue;
            };
            if !AgentTool::is_valid_name(&tool.name) {
                return Err(BlockInfoError::InvalidAgentToolName {
                    block: self.name.clone(),
                    method: ep.method,
                    path: ep.path.clone(),
                    name: tool.name.clone(),
                });
            }
        }
        Ok(())
    }

    /// Set the declared [`crate::InstanceMode`] (default: `PerNode`).
    /// Advisory — the runtime does not enforce it; see the enum docs.
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

    /// Attach the OpenAI-compatible tool descriptor used by agent blocks.
    /// Presence of a descriptor is what marks the block as an agent-callable skill.
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
        let key = format!("{WAFER_RUN_SHARED_PREFIX}APP_NAME");
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
        assert!(msg.contains(WAFER_RUN_SHARED_PREFIX), "message: {msg}");
        assert!(msg.contains("org/b"), "message: {msg}");
        assert!(msg.contains(&key), "message: {msg}");
    }

    #[test]
    fn validate_rejects_reserved_prefix_in_flow_config() {
        let info = BlockInfo::new("org/b", "0.1.0", "iface@v1", "summary").flow_config(vec![
            ConfigVar::new(
                &format!("{WAFER_RUN_SHARED_PREFIX}feature_flag"),
                "desc",
                "",
            ),
        ]);
        assert!(info.validate().is_err());
    }

    #[test]
    fn validate_accepts_a_legal_agent_tool_name() {
        let info = BlockInfo::new("org/b", "0.1.0", "iface@v1", "summary").endpoints(vec![
            BlockEndpoint::get("/b/b/thing").agent_tool("get_thing-v2", "Fetch the thing."),
        ]);
        assert!(info.validate().is_ok());
    }

    /// An MCP client rejects a name outside `[A-Za-z0-9_-]`, and the
    /// rejection is swallowed by the consumer's per-tool try/catch — the
    /// tool just vanishes. Boot is the last place that failure can still be
    /// loud.
    #[test]
    fn validate_rejects_an_illegal_agent_tool_name() {
        for name in [
            "",
            "get thing",
            "get.thing",
            "get/thing",
            "gét_thing",
            &"a".repeat(AgentTool::MAX_NAME_LEN + 1),
        ] {
            let info = BlockInfo::new("org/b", "0.1.0", "iface@v1", "summary").endpoints(vec![
                BlockEndpoint::get("/b/b/thing").agent_tool(name, "Fetch the thing."),
            ]);
            let Err(err) = info.validate() else {
                panic!("name {name:?} must be rejected");
            };
            assert_eq!(
                err,
                BlockInfoError::InvalidAgentToolName {
                    block: "org/b".to_string(),
                    method: HttpMethod::Get,
                    path: "/b/b/thing".to_string(),
                    name: name.to_string(),
                }
            );
            let msg = err.to_string();
            assert!(msg.contains("org/b"), "message: {msg}");
            assert!(msg.contains("/b/b/thing"), "message: {msg}");
            assert!(msg.contains("GET"), "message: {msg}");
        }
    }

    #[test]
    fn validate_ignores_endpoints_that_did_not_opt_in() {
        let info = BlockInfo::new("org/b", "0.1.0", "iface@v1", "summary")
            .endpoints(vec![BlockEndpoint::get("/b/b/thing").summary("no tool")]);
        assert!(info.validate().is_ok());
    }

    #[test]
    fn agent_tool_name_charset_is_the_mcp_one() {
        assert!(AgentTool::is_valid_name("get_product"));
        assert!(AgentTool::is_valid_name("a"));
        assert!(AgentTool::is_valid_name("A-9_z"));
        assert!(AgentTool::is_valid_name(
            &"a".repeat(AgentTool::MAX_NAME_LEN)
        ));

        assert!(!AgentTool::is_valid_name(""));
        assert!(!AgentTool::is_valid_name("has space"));
        assert!(!AgentTool::is_valid_name("has.dot"));
        assert!(!AgentTool::is_valid_name("has:colon"));
        assert!(!AgentTool::is_valid_name("emoji🙂"));
        assert!(!AgentTool::is_valid_name(
            &"a".repeat(AgentTool::MAX_NAME_LEN + 1)
        ));
    }
}
