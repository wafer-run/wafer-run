#![warn(missing_docs)]
//! Runtime introspection block (`wafer-run/inspector`).
//!
//! A read-only HTTP surface over the runtime's sealed state, plus a static
//! HTML UI that renders it. Intended for operators and developers; access
//! defaults to the `admin` role.
//!
//! Three routes, resolved from the last path segment so any mount prefix
//! works:
//!
//! - `{mount}/ui` — the HTML page (`inspector.html`, embedded).
//! - `{mount}/app` — everything the page renders except the agent tools, in
//!   one payload (see [`app_view`]).
//! - `{mount}/webmcp` — the WebMCP tool manifest per auth level (see
//!   [`webmcp::webmcp_view`]).
//!
//! # The data contract
//!
//! `/app` serializes the runtime's own types verbatim — `BlockInfo`,
//! `WaferFlow`, `InterfaceSpec`, and each block's config — and the only
//! transformation applied is [`redacted`]. Every field the page reads is a
//! field on one of those types, so a change to them shows up in the page
//! instead of silently dropping out of it.

mod webmcp;

use std::{collections::HashSet, sync::OnceLock};

use wafer_block::*;

/// Who may use the inspector, resolved once at `lifecycle(Init)` from the
/// two variables [`InspectorBlock::info`] declares.
#[derive(Debug, PartialEq, Eq)]
enum AccessPolicy {
    /// `allow_anonymous` is set: anyone, authenticated or not (dev only).
    Anonymous,
    /// The caller must be authenticated and hold one of these roles.
    Roles(Vec<String>),
}

/// Config key: allow unauthenticated access.
const ALLOW_ANONYMOUS: &str = "allow_anonymous";

/// Config key: comma-separated roles that may use the inspector.
const ALLOWED_ROLES: &str = "allowed_roles";

/// Default of [`ALLOWED_ROLES`].
const DEFAULT_ALLOWED_ROLES: &str = "admin";

/// The roles in a comma-separated list, trimmed, empty entries dropped.
fn parse_roles(list: &str) -> Vec<String> {
    list.split(',')
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(str::to_string)
        .collect()
}

/// The policy that applies when no config overrides it: the declared
/// default of [`ALLOWED_ROLES`].
fn default_policy() -> AccessPolicy {
    AccessPolicy::Roles(parse_roles(DEFAULT_ALLOWED_ROLES))
}

/// Resolve the access policy from block config. A key that is present but
/// malformed — or a role list with no roles, which would lock everyone out —
/// fails Init instead of falling back to a default the operator did not ask
/// for.
fn policy_from_config(config: &BlockConfig) -> Result<AccessPolicy, WaferError> {
    let invalid = |reason: String| WaferError::new(ErrorCode::InvalidArgument, reason);

    let anonymous = match config.get(ALLOW_ANONYMOUS) {
        None => false,
        Some(value) => config.bool(ALLOW_ANONYMOUS).ok_or_else(|| {
            invalid(format!(
                "inspector: `{ALLOW_ANONYMOUS}` must be a boolean, got {value}"
            ))
        })?,
    };
    if anonymous {
        return Ok(AccessPolicy::Anonymous);
    }

    let roles = match config.get(ALLOWED_ROLES) {
        None => return Ok(default_policy()),
        Some(serde_json::Value::String(list)) => parse_roles(list),
        Some(other) => {
            return Err(invalid(format!(
                "inspector: `{ALLOWED_ROLES}` must be a comma-separated string, got {other}"
            )))
        }
    };
    if roles.is_empty() {
        return Err(invalid(format!(
            "inspector: `{ALLOWED_ROLES}` names no roles, which would deny every caller"
        )));
    }
    Ok(AccessPolicy::Roles(roles))
}

impl AccessPolicy {
    /// `Ok` when `msg`'s caller may use the inspector, else the error to
    /// answer with. Reads only the host-owned `auth.*` metadata, which a
    /// WASM guest cannot forge across the trust boundary.
    fn admit(&self, msg: &Message) -> Result<(), WaferError> {
        let Self::Roles(allowed) = self else {
            return Ok(());
        };
        if msg.get_meta("auth.user_id").is_empty() {
            return Err(WaferError::new(
                ErrorCode::Unauthenticated,
                "inspector requires authentication",
            ));
        }
        let held = parse_roles(msg.get_meta("auth.user_roles"));
        if allowed.iter().any(|role| held.contains(role)) {
            Ok(())
        } else {
            Err(WaferError::new(
                ErrorCode::PermissionDenied,
                format!(
                    "inspector requires one of these roles: [{}]",
                    allowed.join(", ")
                ),
            ))
        }
    }
}

/// Read-only HTTP block over the runtime's blocks, flows, configs and
/// interface specs, with a static HTML UI at `{mount}/ui`. See the crate
/// docs for the routes.
pub struct InspectorBlock {
    /// Init-resolved access policy (write-once); [`default_policy`] applies
    /// when no Init has run.
    policy: OnceLock<AccessPolicy>,
}

impl Default for InspectorBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl InspectorBlock {
    /// Construct an inspector. Its access policy is read at `lifecycle(Init)`
    /// from the `allow_anonymous` / `allowed_roles` variables declared in
    /// [`Block::info`]; until then the default (`admin` role) applies.
    pub fn new() -> Self {
        Self {
            policy: OnceLock::new(),
        }
    }
}

/// One of the inspector's routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// `{mount}/ui` — the static HTML UI.
    Ui,
    /// `{mount}/app` — see [`app_view`].
    App,
    /// `{mount}/webmcp` — see [`webmcp::webmcp_view`].
    WebMcp,
}

/// Resolve `path` to a route from its last non-empty segment.
///
/// The block cannot see its own mount prefix — nothing in the message
/// carries it — but every route is exactly one segment below that prefix,
/// so the last segment settles it. Anything else is `None`.
fn route_of(path: &str) -> Option<Route> {
    match path.rsplit('/').find(|segment| !segment.is_empty())? {
        "ui" => Some(Route::Ui),
        "app" => Some(Route::App),
        "webmcp" => Some(Route::WebMcp),
        _ => None,
    }
}

/// What a sensitive config value is replaced with in every inspector view.
const REDACTED: &str = "[redacted]";

/// Key suffixes (compared case-insensitively) that name a secret.
const SENSITIVE_SUFFIXES: &[&str] = &["_SECRET", "_KEY", "_TOKEN", "_PASSWORD"];

/// Whole keys (compared case-insensitively) that name a secret.
const SENSITIVE_KEYS: &[&str] = &["SECRET", "KEY", "TOKEN", "PASSWORD"];

/// Whether a config key names a secret: it is or ends in one of
/// [`SENSITIVE_KEYS`] / [`SENSITIVE_SUFFIXES`] (compared case-insensitively,
/// so a lower-case flow-config key such as `api_key` or `password` counts),
/// or some registered block declares it as a [`InputType::Password`]
/// variable.
fn is_sensitive_key(key: &str, declared: &HashSet<&str>) -> bool {
    let upper = key.to_ascii_uppercase();
    SENSITIVE_KEYS.contains(&upper.as_str())
        || SENSITIVE_SUFFIXES.iter().any(|s| upper.ends_with(s))
        || declared.contains(key)
}

/// What a URL's username and password are replaced with.
const REDACTED_USERINFO: &str = "redacted";

/// `value` with the userinfo of a URL masked (`postgres://u:p@db/x` →
/// `postgres://redacted:redacted@db/x`), or `None` when it is not a URL
/// carrying credentials. A connection string under an innocuous key
/// (`database_url`) is how a password most often reaches a config.
fn masked_url_credentials(value: &str) -> Option<String> {
    let mut url = url::Url::parse(value).ok()?;
    let has_password = url.password().is_some();
    if url.username().is_empty() && !has_password {
        return None;
    }
    url.set_username(REDACTED_USERINFO).ok()?;
    if has_password {
        url.set_password(Some(REDACTED_USERINFO)).ok()?;
    }
    Some(url.to_string())
}

/// Every config key a registered block declares as sensitive
/// ([`ConfigVar::is_sensitive`]), in either `config_keys` or `flow_config`.
fn declared_sensitive_keys(blocks: &[BlockInfo]) -> HashSet<&str> {
    blocks
        .iter()
        .flat_map(|b| b.config_keys.iter().chain(b.flow_config.iter()))
        .filter(|var| var.is_sensitive())
        .map(|var| var.key.as_str())
        .collect()
}

/// A copy of `value` with every sensitive key's value — at any depth, since
/// block configs nest and flow definitions carry per-step `config` objects —
/// replaced by [`REDACTED`] (see [`is_sensitive_key`]), and the credentials
/// of any other string that is a URL masked (see [`masked_url_credentials`]).
fn redacted(value: &serde_json::Value, declared: &HashSet<&str>) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let v = if is_sensitive_key(k, declared) {
                        serde_json::Value::String(REDACTED.to_string())
                    } else {
                        redacted(v, declared)
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(|v| redacted(v, declared)).collect())
        }
        serde_json::Value::String(s) => {
            masked_url_credentials(s).map_or_else(|| value.clone(), serde_json::Value::String)
        }
        other => other.clone(),
    }
}

/// The `/app` payload:
///
/// ```json
/// {
///   "blocks":     [BlockInfo, …],
///   "flows":      [WaferFlow, …],           // redacted
///   "configs":    { "<block>": <config> },  // redacted
///   "interfaces": { "<name>": InterfaceSpec }
/// }
/// ```
///
/// `None` when the runtime does not expose `FlowIntrospection`.
fn app_view(ctx: &dyn Context) -> Option<serde_json::Value> {
    let intro = ctx.flow_introspection()?;
    let blocks = ctx.registered_blocks();
    let declared = declared_sensitive_keys(blocks);
    let flows: Vec<_> = intro
        .flow_defs_json()
        .iter()
        .map(|def| redacted(def, &declared))
        .collect();
    let configs: serde_json::Map<_, _> = ctx
        .block_configs()
        .iter()
        .map(|(name, cfg)| (name.clone(), redacted(cfg, &declared)))
        .collect();
    Some(serde_json::json!({
        "blocks": blocks,
        "flows": flows,
        "configs": configs,
        "interfaces": ctx.interface_specs(),
    }))
}

/// Build an HTML response.
fn html_respond(html: &'static str) -> OutputStream {
    OutputStream::respond_with_meta(
        html.as_bytes().to_vec(),
        vec![MetaEntry {
            key: META_RESP_CONTENT_TYPE.to_string(),
            value: "text/html; charset=utf-8".to_string(),
        }],
    )
}

#[wafer_async_trait]
impl Block for InspectorBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/inspector",
            "0.0.1",
            "http-handler@v1",
            "Runtime introspection — blocks, flows, routes, and agent tools",
        )
        .infrastructure()
        .flow_config(vec![
            ConfigVar::new(
                ALLOW_ANONYMOUS,
                "Let anyone use the inspector, signed in or not. For local \
                 development only: the inspector shows every block's config.",
                "false",
            )
            .name("Allow anonymous access")
            .input_type(InputType::Toggle),
            ConfigVar::new(
                ALLOWED_ROLES,
                "Comma-separated roles that may use the inspector when \
                 anonymous access is off. The caller must be signed in and \
                 hold at least one.",
                DEFAULT_ALLOWED_ROLES,
            )
            .name("Allowed roles"),
        ])
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        // The gate runs before anything is read: every route exposes
        // something an unauthorized caller must not see.
        if let Err(e) = self.policy.get_or_init(default_policy).admit(&msg) {
            return OutputStream::error(e);
        }

        let action = msg.action();
        if !action.is_empty() && action != "retrieve" {
            return OutputStream::error(WaferError::new(
                ErrorCode::Unimplemented,
                "inspector only answers retrieve (GET)",
            ));
        }

        match route_of(msg.path()) {
            Some(Route::Ui) => html_respond(include_str!("inspector.html")),
            Some(Route::App) => match app_view(ctx) {
                Some(view) => ok_json(&view),
                // A request handler must not panic over a host that does not
                // expose the capability, so it is a typed error instead.
                None => OutputStream::error(WaferError::new(
                    ErrorCode::FailedPrecondition,
                    "inspector requires the runtime to expose FlowIntrospection",
                )),
            },
            // A pure projection of `BlockInfo::endpoints`: no flow state, so
            // a runtime without `FlowIntrospection` still serves it.
            Some(Route::WebMcp) => ok_json(&webmcp::webmcp_view(ctx.registered_blocks())),
            None => OutputStream::error(WaferError::new(
                ErrorCode::NotFound,
                "inspector routes are {mount}/ui, {mount}/app and {mount}/webmcp",
            )),
        }
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if let LifecycleType::Init = event.event_type {
            let policy = policy_from_config(&BlockConfig::from_event(&event))?;
            match &policy {
                AccessPolicy::Anonymous => {
                    tracing::warn!("inspector: anonymous access enabled — do not use in production")
                }
                AccessPolicy::Roles(roles) => {
                    tracing::info!("inspector: access restricted to roles: {roles:?}")
                }
            }
            // Write-once: Init fires a single time per registration.
            let _ = self.policy.set(policy);
        }
        Ok(())
    }
}

wafer_block::register_static_block!("wafer-run/inspector", InspectorBlock);

#[cfg(test)]
pub(crate) mod test_support {
    //! A minimal [`Context`] for driving `handle` directly: it carries a
    //! block list and nothing else, so `flow_introspection()` stays `None`.

    use wafer_block::{
        streams::{input::InputStream, output::TerminalNotResponse},
        Block, BlockInfo, Context, ErrorCode, Message,
    };

    use super::InspectorBlock;

    #[derive(Clone, Default)]
    pub struct BlocksContext {
        pub blocks: Vec<BlockInfo>,
    }

    #[async_trait::async_trait]
    impl Context for BlocksContext {
        async fn call_block(
            &self,
            _name: &str,
            _msg: Message,
            _input: InputStream,
        ) -> wafer_block::streams::output::OutputStream {
            wafer_block::streams::output::OutputStream::error(wafer_block::WaferError::new(
                ErrorCode::Unimplemented,
                "mock",
            ))
        }
        fn is_cancelled(&self) -> bool {
            false
        }
        fn config_get(&self, _key: &str) -> Option<&str> {
            None
        }
        fn registered_blocks(&self) -> &[BlockInfo] {
            &self.blocks
        }
        fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
            std::sync::Arc::new(self.clone())
        }
        // Denies every access, as the trait's default `check_resource_access` does.
        fn resource_access_admitted(
            &self,
            _resource: &str,
            _resource_type: wafer_block::types::ResourceType,
            _access: wafer_block::types::ResourceAccess,
        ) -> bool {
            false
        }
    }

    /// `GET path` through a default-policy inspector on `ctx`.
    pub async fn get(
        ctx: &BlocksContext,
        path: &str,
        meta: &[(&str, &str)],
    ) -> Result<Vec<u8>, ErrorCode> {
        let mut msg = Message::new("retrieve");
        msg.set_meta(wafer_block::meta::META_REQ_RESOURCE, path);
        for (k, v) in meta {
            msg.set_meta(*k, *v);
        }
        let out = InspectorBlock::new()
            .handle(ctx, msg, InputStream::empty())
            .await;
        match out.collect_buffered().await {
            Ok(response) => Ok(response.body),
            Err(TerminalNotResponse::Error(e)) => Err(e.code),
            Err(other) => panic!("unexpected terminal: {other:?}"),
        }
    }

    /// Metadata of a signed-in caller holding the `admin` role.
    pub fn admin() -> [(&'static str, &'static str); 2] {
        [("auth.user_id", "u1"), ("auth.user_roles", "admin")]
    }
}

#[cfg(test)]
mod auth_tests {
    //! SEC-01: the inspector authorizes solely from message metadata
    //! (`auth.user_id` / `auth.user_roles`). Since a WASM guest cannot forge
    //! those keys across the trust boundary (host-owned protected namespace,
    //! enforced in `wasmi_loader`), these tests pin the authorization decision
    //! itself and the config that sets it.

    use wafer_block::{BlockConfig, ErrorCode, LifecycleEvent, LifecycleType};

    use super::{
        policy_from_config,
        test_support::{get, BlocksContext},
        AccessPolicy,
    };

    async fn code(meta: &[(&str, &str)]) -> ErrorCode {
        // `/app` on a context without FlowIntrospection: a caller that passes
        // the gate gets FailedPrecondition, which is how these tests tell
        // "authorized" from "denied".
        get(&BlocksContext::default(), "/_inspector/app", meta)
            .await
            .expect_err("the mock context never produces a response")
    }

    #[tokio::test]
    async fn default_policy_denies_unauthenticated() {
        assert_eq!(code(&[]).await, ErrorCode::Unauthenticated);
    }

    #[tokio::test]
    async fn default_policy_denies_wrong_role() {
        let meta = [
            ("auth.user_id", "u1"),
            ("auth.user_roles", "viewer,developer"),
        ];
        assert_eq!(code(&meta).await, ErrorCode::PermissionDenied);
    }

    #[tokio::test]
    async fn default_policy_admits_admin_role() {
        let meta = [
            ("auth.user_id", "u1"),
            ("auth.user_roles", " viewer , admin "),
        ];
        assert_eq!(code(&meta).await, ErrorCode::FailedPrecondition);
    }

    #[tokio::test]
    async fn role_parsing_ignores_empty_and_whitespace_entries() {
        let meta = [("auth.user_id", "u1"), ("auth.user_roles", " , ,  ")];
        assert_eq!(code(&meta).await, ErrorCode::PermissionDenied);
    }

    #[tokio::test]
    async fn the_gate_runs_before_routing() {
        // An unknown path must not tell an outsider anything either.
        let denied = get(&BlocksContext::default(), "/_inspector/nope", &[]).await;
        assert_eq!(denied, Err(ErrorCode::Unauthenticated));
    }

    fn policy(config: &serde_json::Value) -> Result<AccessPolicy, ErrorCode> {
        let event = LifecycleEvent {
            event_type: LifecycleType::Init,
            data: serde_json::to_vec(config).expect("config JSON"),
        };
        policy_from_config(&BlockConfig::from_event(&event)).map_err(|e| e.code)
    }

    #[test]
    fn config_resolves_to_the_declared_policy() {
        let admin = AccessPolicy::Roles(vec!["admin".into()]);
        assert_eq!(policy(&serde_json::json!({})), Ok(admin));
        assert_eq!(
            policy(&serde_json::json!({ "allow_anonymous": true })),
            Ok(AccessPolicy::Anonymous)
        );
        // Flow-step config arrives stringified.
        assert_eq!(
            policy(&serde_json::json!({ "allow_anonymous": "true" })),
            Ok(AccessPolicy::Anonymous)
        );
        assert_eq!(
            policy(&serde_json::json!({ "allow_anonymous": false, "allowed_roles": "ops, dev" })),
            Ok(AccessPolicy::Roles(vec!["ops".into(), "dev".into()]))
        );
    }

    #[test]
    fn malformed_config_fails_init_instead_of_falling_back() {
        for bad in [
            serde_json::json!({ "allow_anonymous": "yes" }),
            serde_json::json!({ "allowed_roles": ["admin"] }),
            serde_json::json!({ "allowed_roles": " , " }),
        ] {
            assert_eq!(policy(&bad), Err(ErrorCode::InvalidArgument), "{bad}");
        }
    }

    #[test]
    fn routes_resolve_from_the_last_segment_under_any_mount() {
        use super::{route_of, Route};
        assert_eq!(route_of("/_inspector/ui"), Some(Route::Ui));
        assert_eq!(route_of("/b/inspector/app/"), Some(Route::App));
        assert_eq!(route_of("/a/b/c/webmcp"), Some(Route::WebMcp));
        assert_eq!(route_of("/_inspector"), None);
        assert_eq!(route_of("/_inspector/blocks"), None);
        assert_eq!(route_of("/"), None);
    }
}

#[cfg(test)]
mod redaction_tests {
    //! `/app` serves block configs and flow definitions as the runtime holds
    //! them; a secret an operator put in either must not reach the page.
    //! Driven through a real `Wafer`, so the configs and the flow definitions
    //! are the runtime's own snapshot, not a mock's.

    use std::sync::Arc;

    use wafer_block::{streams::input::InputStream, *};
    use wafer_block_macro::wafer_async_trait;

    use super::InspectorBlock;

    /// Declares one `Password`-typed variable whose key matches neither
    /// suffix rule, so only the declaration can mark it sensitive.
    struct Vault;

    #[wafer_async_trait]
    impl Block for Vault {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test-org/vault", "0.0.1", "http-handler@v1", "test").config_keys(vec![
                ConfigVar::new("TEST_ORG__VAULT__PASSPHRASE", "passphrase", "")
                    .input_type(InputType::Password),
            ])
        }

        async fn handle(&self, _ctx: &dyn Context, msg: Message, _: InputStream) -> OutputStream {
            OutputStream::continue_with(msg)
        }
    }

    const FLOW: &str = r#"{
        "id": "test-flow",
        "name": "Test",
        "version": "0.1.0",
        "steps": [
            { "id": "vault", "block": "test-org/vault", "config": { "api_key": "fl0wk3y", "mode": "plain" } }
        ]
    }"#;

    #[tokio::test]
    async fn app_view_redacts_sensitive_config_values() {
        let mut wafer = wafer_run::Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .build()
            .expect("empty wafer");
        wafer
            .register_block("wafer-run/inspector", Arc::new(InspectorBlock::new()))
            .expect("register inspector");
        wafer
            .register_block("test-org/vault", Arc::new(Vault))
            .expect("register vault");
        wafer.add_block_config(
            "wafer-run/inspector",
            serde_json::json!({ "allow_anonymous": true }),
        );
        wafer.add_block_config(
            "test-org/vault",
            serde_json::json!({
                "X_SECRET": "s3cr3t",
                "TEST_ORG__VAULT__PASSPHRASE": "hunter2",
                "nested": { "db_key": "k3y", "GITHUB_TOKEN": "gh0tok", "password": "pw0rd" },
                "SMTP_PASSWORD": "smtpp4ss",
                "database_url": "postgres://dbuser:dbp4ss@db.internal:5432/app",
                "git_remote": "https://gh0user@github.com/o/r.git",
                "plain": "visible-value"
            }),
        );
        wafer.add_flow_json(FLOW).expect("flow");
        let wafer = wafer.start().await.expect("start");

        let path = "/_inspector/app";
        let mut msg = Message::new(format!("GET:{path}"));
        msg.set_meta(META_REQ_ACTION, "retrieve");
        msg.set_meta(META_REQ_RESOURCE, path);
        let resp = wafer
            .run_block("wafer-run/inspector", msg, InputStream::empty())
            .await
            .collect_buffered()
            .await
            .expect("inspector answers");
        let app = String::from_utf8(resp.body).expect("utf-8 JSON");

        for secret in [
            "s3cr3t", "hunter2", "k3y", "fl0wk3y", "gh0tok", "pw0rd", "smtpp4ss", "dbuser",
            "dbp4ss", "gh0user",
        ] {
            assert!(!app.contains(secret), "{secret} leaked: {app}");
        }
        assert!(
            app.contains("visible-value"),
            "non-secret config is still shown: {app}"
        );
        assert!(
            app.contains("X_SECRET"),
            "the key stays, only its value goes: {app}"
        );
        assert!(
            app.contains("postgres://redacted:redacted@db.internal:5432/app"),
            "a URL keeps everything but its credentials: {app}"
        );
        assert!(
            app.contains("\"mode\":\"plain\""),
            "non-secret step config stays: {app}"
        );
    }
}
