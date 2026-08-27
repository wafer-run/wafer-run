#![warn(missing_docs)]
//! Runtime introspection block (`wafer-run/inspector`).
//!
//! Exposes a read-only HTTP surface that lists the runtime's registered blocks,
//! flows, and interface specs, plus a small static HTML UI for browsing them.
//! Intended for operators and developers; access defaults to the `admin` role.

use std::sync::OnceLock;

use wafer_block::*;

/// Access control policy for the inspector.
#[expect(
    dead_code,
    reason = "Authenticated variant reserved for future config path; \
              other variants are constructed from config"
)]
enum AccessPolicy {
    /// Require `auth.user_id` to be set (default).
    Authenticated,
    /// Allow unauthenticated access (dev mode).
    Anonymous,
    /// Require `auth.user_roles` to contain one of these roles.
    Roles(Vec<String>),
}

/// The policy applied when `lifecycle(Init)` supplied no override:
/// require the `admin` role.
fn default_policy() -> AccessPolicy {
    AccessPolicy::Roles(vec!["admin".to_string()])
}

/// Read-only HTTP block that exposes the runtime's registered blocks, flows,
/// and interface specs as JSON, plus a small HTML UI at `/ui`.
///
/// Routes (resolved from the tail of the message path so any mount prefix
/// works — see [`Route`]): `/app`, `/blocks`, `/blocks/{name}`, `/flows`,
/// `/flows/{id}`, `/interfaces`, `/webmcp`, `/ui`; any other path returns a
/// counts summary.
pub struct InspectorBlock {
    /// Init-resolved access policy (write-once); [`default_policy`] applies
    /// when Init supplied no override. Same `OnceLock` pattern as the other
    /// infrastructure blocks.
    policy: OnceLock<AccessPolicy>,
}

impl Default for InspectorBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl InspectorBlock {
    /// Construct an inspector with the default `admin`-role access policy.
    ///
    /// Override at lifecycle `Init` time by passing config of the shape
    /// `{ "allow_anonymous": true }` or `{ "allowed_roles": ["..."] }`.
    pub fn new() -> Self {
        Self {
            policy: OnceLock::new(),
        }
    }
}

/// One of the inspector's own routes, resolved from the tail of a request
/// path.
///
/// # Why the tail and not a whole-path suffix
///
/// The block is suffix-routed because it cannot see its own mount prefix —
/// nothing in the message carries it. But every route is exactly one segment
/// after that prefix, except `/blocks/{name}` and `/flows/{id}` which are
/// two, so the last one or two segments settle which route was asked for
/// without knowing where the mount begins.
///
/// Matching whole-path suffixes cannot do that: `ends_with("/webmcp")` is
/// equally true of `{mount}/webmcp` and of `{mount}/blocks/webmcp` — a block
/// whose name happens to be `webmcp` — so whichever `ends_with` runs first
/// silently shadows the other, and the order of the checks becomes load-
/// bearing. Reading the parent segment makes the two cases distinguishable
/// and the order of the arms irrelevant.
///
/// The one shape this still cannot resolve is a mount prefix that itself
/// ends in `/blocks` or `/flows`, where `{mount}/webmcp` and
/// `{mount}/blocks/{name}` are the same string. No suffix rule can, and
/// nothing mounts the inspector there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route<'a> {
    /// `{mount}/app` — flows, configs, blocks, and interfaces in one payload.
    App,
    /// `{mount}/blocks` — every registered block.
    Blocks,
    /// `{mount}/blocks/{name}` — one block, by name.
    Block(&'a str),
    /// `{mount}/flows` — every flow.
    Flows,
    /// `{mount}/flows/{id}` — one flow definition, by id.
    Flow(&'a str),
    /// `{mount}/interfaces` — every interface spec.
    Interfaces,
    /// `{mount}/webmcp` — the agent tool manifest per auth level.
    WebMcp,
    /// `{mount}/ui` — the static HTML UI.
    Ui,
    /// Anything else, including the mount root: a counts summary.
    Summary,
}

/// Resolve `path` to one of the inspector's routes. See [`Route`].
fn route_of(path: &str) -> Route<'_> {
    let mut tail = path.rsplit('/').filter(|segment| !segment.is_empty());
    let Some(last) = tail.next() else {
        return Route::Summary;
    };
    match (tail.next(), last) {
        (Some("blocks"), name) => Route::Block(name),
        (Some("flows"), id) => Route::Flow(id),
        (_, "app") => Route::App,
        (_, "blocks") => Route::Blocks,
        (_, "flows") => Route::Flows,
        (_, "interfaces") => Route::Interfaces,
        (_, "webmcp") => Route::WebMcp,
        (_, "ui") => Route::Ui,
        _ => Route::Summary,
    }
}

/// The auth levels the WebMCP view renders a manifest for, in increasing
/// order of privilege.
const WEBMCP_LEVELS: [AuthLevel; 3] = [
    AuthLevel::Public,
    AuthLevel::Authenticated,
    AuthLevel::Admin,
];

/// Build the `/webmcp` payload: the WebMCP tool manifest as each auth level
/// receives it, plus, per level, the endpoints that opted in to agent-tool
/// exposure and produced no tool.
///
/// # Why one document per level rather than one manifest
///
/// The point the view has to make is that the same site presents a
/// *different* tool surface depending on who is asking. A single manifest
/// cannot show that; three side by side make it unmissable, and the diff
/// between adjacent levels is what an operator actually wants to audit
/// before shipping an agent surface.
///
/// Refusals are per level for the same reason. All but one refusal reason is
/// a defect in an endpoint's own declarations and is identical at every
/// level, so those three lists agree — but `DuplicateToolName` is scoped to
/// the manifest the collision occurs in, so a name claimed by both a public
/// and an admin endpoint is a refusal only at the admin level, and the
/// public tool below it is published. Folding the lists together would hide
/// exactly that.
///
/// # A level's refusals name only endpoints that level can see
///
/// The producer reports a structural refusal to *every* caller, because a
/// malformed path template is a defect whether an anonymous visitor or an
/// admin asked, and an author debugging one should not have to authenticate
/// to see it. That is right for the operator's log and wrong here: this page
/// says it shows the manifest as each caller receives it, so an admin-only
/// endpoint's block, method, path and tool name appearing under the "Public"
/// heading is both a false statement about what that caller receives and a
/// disclosure across exactly the tier boundary the auth filter draws.
///
/// So each level keeps only the entries whose
/// `WebMcpRefusalReport::visible_to_caller` is set. That flag is the
/// producer's own filter decision travelling with the report; re-deriving it
/// here would be a second implementation of a security-critical filter, the
/// same reason this block depends on `wafer-core` instead of rebuilding the
/// refusal wall in the page's JavaScript.
///
/// # The auth basis is `ep.auth`, and the view says so
///
/// This block cannot see the consumer's route table, so it projects each
/// endpoint's *declared* auth level. A consumer whose router raises auth by
/// prefix — enforcing `max(prefix_tier, ep.auth)` — serves a narrower
/// surface than what is rendered here. That is exactly the assumption
/// `wafer_core::discovery::generate_webmcp_declared_auth` is named for, and
/// the rendered page carries the caveat rather than leaving a reader to
/// assume the inspector is showing production truth.
///
/// The same caveat covers feature gating, and for the same reason: a
/// consumer that serves the manifest for only its feature-enabled blocks
/// (Impresspress does) shows a narrower surface again, and `registered_blocks`
/// carries no live enablement state for this block to filter on —
/// `BlockInfo::default_enabled` is a first-run default, not what the admin
/// has since switched off. Rather than filter on something that is not the
/// answer, the page states plainly that it ignores feature gating.
fn webmcp_view(blocks: &[BlockInfo]) -> serde_json::Value {
    use wafer_core::discovery::WebMcpRefusalScope;

    let levels: Vec<serde_json::Value> = WEBMCP_LEVELS
        .iter()
        .map(|level| {
            let (manifest, refusals) =
                wafer_core::discovery::generate_webmcp_report(blocks, *level, |_block, ep| ep.auth);
            // See "A level's refusals name only endpoints that level can
            // see" above.
            let visible: Vec<_> = refusals
                .into_iter()
                .filter(|r| r.visible_to_caller)
                .collect();

            let published = manifest["tools"].as_array().map_or(0, Vec::len);
            // How many endpoints opted in *at this level*. An opted-in
            // endpoint this caller can see either becomes a tool or is
            // refused one, and nothing else, so the two add up — and the
            // column's "N of M" then compares like with like. Dividing by a
            // deployment-wide total instead makes every column below the
            // top read as though its own endpoints had failed.
            let opted_in = published
                + visible
                    .iter()
                    .filter(|r| r.scope == WebMcpRefusalScope::Tool)
                    .count();

            let refusals: Vec<serde_json::Value> = visible
                .into_iter()
                .map(|r| {
                    serde_json::json!({
                        "block": r.block,
                        "method": r.method.to_string(),
                        "path": r.path,
                        "tool_name": r.tool_name,
                        "scope": r.scope.to_string(),
                        "reason": r.reason.to_string(),
                    })
                })
                .collect();

            serde_json::json!({
                "level": level.to_string(),
                "manifest": manifest,
                "refusals": refusals,
                "opted_in": opted_in,
            })
        })
        .collect();

    // Every endpoint that asked to be a tool, anywhere in the deployment.
    // Counted from the declarations rather than from the manifests, because
    // the whole question is what did not arrive. This is the page-level
    // figure; each column carries its own, because the two answer different
    // questions.
    let opted_in = blocks
        .iter()
        .flat_map(|b| b.endpoints.iter())
        .filter(|ep| ep.is_agent_tool())
        .count();

    serde_json::json!({
        "auth_basis": "declared",
        "feature_gating": "ignored",
        "opted_in_endpoints": opted_in,
        "levels": levels,
    })
}

/// Build an HTML OutputStream response.
fn html_respond(html: Vec<u8>) -> OutputStream {
    OutputStream::respond_with_meta(
        html,
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
            "Runtime introspection — blocks, flows, and visual UI",
        )
        .infrastructure()
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        // Access control
        {
            let policy = self.policy.get_or_init(default_policy);
            match policy {
                AccessPolicy::Anonymous => {}
                AccessPolicy::Authenticated => {
                    if msg.get_meta("auth.user_id").is_empty() {
                        return OutputStream::error(WaferError {
                            code: ErrorCode::Unauthenticated,
                            message: "inspector requires authentication".to_string(),
                            meta: vec![],
                        });
                    }
                }
                AccessPolicy::Roles(allowed) => {
                    if msg.get_meta("auth.user_id").is_empty() {
                        return OutputStream::error(WaferError {
                            code: ErrorCode::Unauthenticated,
                            message: "inspector requires authentication".to_string(),
                            meta: vec![],
                        });
                    }
                    let user_roles: Vec<&str> = msg
                        .get_meta("auth.user_roles")
                        .split(',')
                        .map(|r| r.trim())
                        .filter(|r| !r.is_empty())
                        .collect();
                    if !allowed.iter().any(|a| user_roles.contains(&a.as_str())) {
                        let roles_list = allowed.join(", ");
                        return OutputStream::error(WaferError {
                            code: ErrorCode::PermissionDenied,
                            message: format!(
                                "inspector requires one of these roles: [{roles_list}]"
                            ),
                            meta: vec![],
                        });
                    }
                }
            }
        }

        // Only allow retrieve (GET)
        let action = msg.action().to_string();
        if !action.is_empty() && action != "retrieve" {
            return OutputStream::error(WaferError {
                code: ErrorCode::Unimplemented,
                message: "only retrieve action is allowed".to_string(),
                meta: vec![],
            });
        }

        let path = msg.path().to_string();

        // Flow data is exposed via the runtime's FlowIntrospection
        // capability, and each arm that needs it binds it for itself. Binding
        // once for the whole handler instead would make every route that does
        // *not* need it — the WebMCP view, the block routes, the static UI —
        // have to be answered above the bind, so route order would carry a
        // requirement that has nothing to do with routing. That is how
        // `/webmcp` ended up ahead of everything else and shadowing
        // `/blocks/webmcp`.
        match route_of(&path) {
            // A pure projection of `BlockInfo::endpoints`: no live flow
            // state, so a runtime without that capability still serves it.
            Route::WebMcp => ok_json(&webmcp_view(ctx.registered_blocks())),

            Route::App => {
                let Some(intro) = ctx.flow_introspection() else {
                    return no_flow_introspection();
                };
                let flows = intro.flow_defs_json();
                let configs = ctx.block_configs();
                let blocks = ctx.registered_blocks();
                let interfaces = ctx.interface_specs();
                ok_json(&serde_json::json!({
                    "flows": flows,
                    "configs": configs,
                    "blocks": blocks,
                    "interfaces": interfaces,
                }))
            }

            Route::Blocks => ok_json(&ctx.registered_blocks()),

            Route::Block(name) => {
                let decoded = percent_encoding::percent_decode_str(name)
                    .decode_utf8_lossy()
                    .into_owned();
                let blocks = ctx.registered_blocks();
                match blocks.iter().find(|b| b.name == decoded) {
                    Some(info) => ok_json(info),
                    None => OutputStream::error(WaferError {
                        code: ErrorCode::NotFound,
                        message: format!("block '{decoded}' not found"),
                        meta: vec![],
                    }),
                }
            }

            Route::Flows => {
                let Some(intro) = ctx.flow_introspection() else {
                    return no_flow_introspection();
                };
                ok_json(&intro.flow_infos_json())
            }

            Route::Flow(id) => {
                let Some(intro) = ctx.flow_introspection() else {
                    return no_flow_introspection();
                };
                let decoded = percent_encoding::percent_decode_str(id)
                    .decode_utf8_lossy()
                    .into_owned();
                match intro
                    .flow_defs_json()
                    .into_iter()
                    .find(|c| c.get("id").and_then(|v| v.as_str()) == Some(decoded.as_str()))
                {
                    Some(def) => ok_json(&def),
                    None => OutputStream::error(WaferError {
                        code: ErrorCode::NotFound,
                        message: format!("flow '{decoded}' not found"),
                        meta: vec![],
                    }),
                }
            }

            Route::Interfaces => ok_json(&ctx.interface_specs()),

            Route::Ui => html_respond(include_str!("inspector.html").as_bytes().to_vec()),

            Route::Summary => {
                let Some(intro) = ctx.flow_introspection() else {
                    return no_flow_introspection();
                };
                let blocks = ctx.registered_blocks();
                let flows = intro.flow_infos_json();
                ok_json(&serde_json::json!({
                    "block_count": blocks.len(),
                    "flow_count": flows.len(),
                    "blocks": blocks.iter().map(|b| &b.name).collect::<Vec<_>>(),
                    "flows": flows
                        .iter()
                        .filter_map(|c| c.get("id").and_then(|v| v.as_str()))
                        .collect::<Vec<_>>(),
                }))
            }
        }
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if let LifecycleType::Init = event.event_type {
            let config = BlockConfig::from_event(&event);
            // "allow_anonymous": true  → anyone can access
            if config.bool("allow_anonymous").unwrap_or(false) {
                tracing::warn!("inspector: anonymous access enabled — do not use in production");
                // Write-once: Init fires a single time per registration.
                let _ = self.policy.set(AccessPolicy::Anonymous);
            }
            // "allowed_roles": ["admin", "developer"]  → only these roles
            else if let Some(role_list) = config.str_array("allowed_roles") {
                if !role_list.is_empty() {
                    tracing::info!("inspector: access restricted to roles: {:?}", role_list);
                    let _ = self.policy.set(AccessPolicy::Roles(role_list));
                }
            }
        }
        Ok(())
    }
}

/// The inspector is a runtime-only block, so an absent `FlowIntrospection`
/// capability is a misconfiguration — but a request handler must not panic
/// (and thus abort the worker) over a config mismatch, so it is a typed
/// error instead.
fn no_flow_introspection() -> OutputStream {
    OutputStream::error(WaferError::new(
        ErrorCode::FailedPrecondition,
        "inspector requires the runtime to expose FlowIntrospection",
    ))
}

wafer_block::register_static_block!("wafer-run/inspector", InspectorBlock);

#[cfg(test)]
mod auth_tests {
    //! SEC-01: the inspector authorizes solely from message metadata
    //! (`auth.user_id` / `auth.user_roles`). Since a WASM guest cannot forge
    //! those keys across the trust boundary (host-owned protected namespace,
    //! enforced in `wasmi_loader`), these tests pin the authorization decision
    //! itself: default policy denies the unauthenticated and the wrong-role
    //! caller, and the role list is parsed with comma-split + trim + empty
    //! filtering.

    use wafer_block::{
        streams::{input::InputStream, output::TerminalNotResponse},
        Context, ErrorCode, Message,
    };

    use super::InspectorBlock;

    /// Minimal Context: the authorization arm returns before any Context
    /// method is consulted, and a caller that *passes* auth then hits
    /// `flow_introspection()` (defaulted to `None`) → `FailedPrecondition`,
    /// which is exactly the signal these tests use to prove "auth passed".
    #[derive(Clone)]
    struct MockContext;

    #[async_trait::async_trait]
    impl Context for MockContext {
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
        fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
            std::sync::Arc::new(self.clone())
        }
    }

    async fn handle_code(msg: Message) -> ErrorCode {
        use wafer_block::Block;
        let block = InspectorBlock::new();
        let out = block.handle(&MockContext, msg, InputStream::empty()).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => e.code,
            Ok(_) => panic!("inspector must not produce a Respond on a mock context"),
            Err(other) => panic!("unexpected terminal: {other:?}"),
        }
    }

    #[tokio::test]
    async fn default_policy_denies_unauthenticated() {
        // No auth.user_id → Unauthenticated, before any data is exposed.
        let code = handle_code(Message::new("retrieve")).await;
        assert_eq!(code, ErrorCode::Unauthenticated);
    }

    #[tokio::test]
    async fn default_policy_denies_wrong_role() {
        // Authenticated but roles lack `admin` → PermissionDenied.
        let mut msg = Message::new("retrieve");
        msg.set_meta("auth.user_id", "u1");
        msg.set_meta("auth.user_roles", "viewer,developer");
        assert_eq!(handle_code(msg).await, ErrorCode::PermissionDenied);
    }

    #[tokio::test]
    async fn default_policy_admits_admin_role() {
        // `admin` present among comma+whitespace-separated roles → auth passes;
        // the next failure is FailedPrecondition (mock has no FlowIntrospection),
        // which distinguishes "authorized" from "denied".
        let mut msg = Message::new("retrieve");
        msg.set_meta("auth.user_id", "u1");
        msg.set_meta("auth.user_roles", " viewer , admin ");
        assert_eq!(handle_code(msg).await, ErrorCode::FailedPrecondition);
    }

    #[tokio::test]
    async fn role_parsing_ignores_empty_and_whitespace_entries() {
        // Empty/whitespace fragments must not accidentally match; a role list
        // of only separators leaves no roles → wrong-role denial.
        let mut msg = Message::new("retrieve");
        msg.set_meta("auth.user_id", "u1");
        msg.set_meta("auth.user_roles", " , ,  ");
        assert_eq!(handle_code(msg).await, ErrorCode::PermissionDenied);
    }
}

#[cfg(test)]
mod webmcp_tests {
    //! The WebMCP view is the inspector's only page whose content is a
    //! security claim: it says what tool surface each caller is shown. These
    //! pin the claim rather than the markup — that the three columns really
    //! differ, that an endpoint which opted in and produced no tool is named
    //! with a reason, and that the route does not need the runtime's flow
    //! capability to answer.

    use wafer_block::{
        streams::{input::InputStream, output::TerminalNotResponse},
        AuthLevel, Block, BlockEndpoint, BlockInfo, Context, ErrorCode, Message,
    };

    use super::InspectorBlock;

    #[derive(Clone)]
    struct BlocksContext {
        blocks: Vec<BlockInfo>,
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
    }

    /// One tool per level, plus a refused endpoint at *each* of the outer two
    /// levels.
    ///
    /// The admin-only refusal is the point: a structural refusal is computed
    /// before the auth filter, so without a per-level filter the public
    /// column would name an admin-only endpoint's block, method, path and
    /// tool name. A fixture whose only refusal is public passes either way
    /// and pins nothing.
    fn fixture() -> Vec<BlockInfo> {
        vec![
            BlockInfo::new("test/shop", "1.0.0", "http-handler@v1", "Shop").endpoints(vec![
                BlockEndpoint::get("/b/shop/products")
                    .auth(AuthLevel::Public)
                    .agent_tool("list_products", "List the public catalogue."),
                BlockEndpoint::get("/b/shop/orders")
                    .auth(AuthLevel::Authenticated)
                    .agent_tool("list_my_orders", "List the signed-in user's orders."),
                BlockEndpoint::get("/b/shop/admin/users")
                    .auth(AuthLevel::Admin)
                    .agent_tool("list_users", "List every account."),
                BlockEndpoint::get("/b/shop/broken/{id}")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_broken", "Never becomes a tool."),
                BlockEndpoint::get("/b/shop/admin/broken/{secret_id}")
                    .auth(AuthLevel::Admin)
                    .agent_tool("get_admin_broken", "Never becomes a tool either."),
            ]),
        ]
    }

    /// `GET {mount}/webmcp` through the real handler, as an admin.
    async fn webmcp_body(blocks: Vec<BlockInfo>) -> serde_json::Value {
        let mut msg = Message::new("retrieve");
        msg.set_meta(wafer_block::meta::META_REQ_RESOURCE, "/_inspector/webmcp");
        msg.set_meta("auth.user_id", "u1");
        msg.set_meta("auth.user_roles", "admin");
        let out = InspectorBlock::new()
            .handle(&BlocksContext { blocks }, msg, InputStream::empty())
            .await;
        match out.collect_buffered().await {
            Ok(response) => serde_json::from_slice(&response.body).expect("valid JSON body"),
            Err(TerminalNotResponse::Error(e)) => panic!("unexpected error: {e:?}"),
            Err(other) => panic!("unexpected terminal: {other:?}"),
        }
    }

    fn refusals_at(body: &serde_json::Value, level: &str) -> Vec<serde_json::Value> {
        body["levels"]
            .as_array()
            .expect("levels")
            .iter()
            .find(|l| l["level"] == level)
            .unwrap_or_else(|| panic!("level {level}"))["refusals"]
            .as_array()
            .expect("refusals")
            .clone()
    }

    fn names_at(body: &serde_json::Value, level: &str) -> Vec<String> {
        body["levels"]
            .as_array()
            .expect("levels")
            .iter()
            .find(|l| l["level"] == level)
            .unwrap_or_else(|| panic!("level {level}"))["manifest"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|t| t["name"].as_str().expect("name").to_string())
            .collect()
    }

    #[tokio::test]
    async fn webmcp_view_answers_without_the_flow_introspection_capability() {
        // Every other data route binds `ctx.flow_introspection()` first and
        // fails with FailedPrecondition when it is absent. The WebMCP view is
        // a pure projection of BlockInfo, so it must answer regardless.
        let body = webmcp_body(fixture()).await;
        assert_eq!(body["auth_basis"], "declared");
        assert_eq!(body["feature_gating"], "ignored");
        assert_eq!(body["opted_in_endpoints"], 5);
    }

    #[tokio::test]
    async fn webmcp_view_shows_a_wider_tool_surface_at_each_level() {
        // The point the page has to make at a glance: the same site presents
        // a different tool surface depending on who is asking.
        let body = webmcp_body(fixture()).await;
        assert_eq!(names_at(&body, "public"), vec!["list_products".to_string()]);
        assert_eq!(
            names_at(&body, "authenticated"),
            vec!["list_products".to_string(), "list_my_orders".to_string()]
        );
        assert_eq!(
            names_at(&body, "admin"),
            vec![
                "list_products".to_string(),
                "list_my_orders".to_string(),
                "list_users".to_string()
            ]
        );

        // And a name a caller cannot use never reaches that caller's column —
        // the same posture the producer's auth filter enforces.
        let public_level = body["levels"][0].to_string();
        assert!(
            !public_level.contains("list_users") && !public_level.contains("/b/shop/admin/users"),
            "the admin tool leaked into the public column: {public_level}"
        );
    }

    #[tokio::test]
    async fn webmcp_view_does_not_name_an_admin_endpoint_in_a_lower_column() {
        // A structural refusal is computed *before* the auth filter, so
        // without a per-level filter the admin-only endpoint's block, method,
        // path and tool name land in the public column — the same cross-tier
        // disclosure the auth filter exists to close, on a page that claims
        // to show the manifest as each caller receives it.
        let body = webmcp_body(fixture()).await;

        for (index, level) in ["public", "authenticated"].iter().enumerate() {
            let rendered = body["levels"][index].to_string();
            assert_eq!(body["levels"][index]["level"], *level);
            for leaked in [
                "get_admin_broken",
                "/b/shop/admin/broken/{secret_id}",
                "secret_id",
            ] {
                assert!(
                    !rendered.contains(leaked),
                    "the admin-only refusal leaked into the {level} column via \
                     `{leaked}`: {rendered}"
                );
            }
        }

        // It is still reported where it belongs — the filter must not have
        // simply dropped the diagnostic.
        let admin_refusals = refusals_at(&body, "admin");
        assert!(
            admin_refusals
                .iter()
                .any(|r| r["tool_name"] == "get_admin_broken"),
            "the admin column must still carry it: {admin_refusals:?}"
        );
    }

    #[tokio::test]
    async fn webmcp_view_names_an_opted_in_endpoint_that_produced_no_tool() {
        let body = webmcp_body(fixture()).await;
        for level in ["public", "authenticated", "admin"] {
            let refusals = refusals_at(&body, level);
            let broken = refusals
                .iter()
                .find(|r| r["tool_name"] == "get_broken")
                .unwrap_or_else(|| panic!("{level}: {refusals:?}"));
            assert_eq!(broken["path"], "/b/shop/broken/{id}");
            assert_eq!(broken["scope"], "tool");
            assert!(
                broken["reason"]
                    .as_str()
                    .expect("reason")
                    .contains("path template placeholders"),
                "the producer's own reason must reach the page: {refusals:?}"
            );
        }
    }

    #[tokio::test]
    async fn webmcp_view_counts_opted_in_endpoints_per_level() {
        // "N of M" must compare like with like. Against a deployment-wide M
        // every column below the top reads as though its own endpoints had
        // failed, when in fact they belong to a tier it cannot see.
        let body = webmcp_body(fixture()).await;
        let opted_in = |level: &str| {
            body["levels"]
                .as_array()
                .expect("levels")
                .iter()
                .find(|l| l["level"] == level)
                .expect("level")["opted_in"]
                .as_u64()
                .expect("opted_in")
        };
        // public: list_products + get_broken.
        assert_eq!(opted_in("public"), 2);
        // authenticated: + list_my_orders.
        assert_eq!(opted_in("authenticated"), 3);
        // admin: + list_users + get_admin_broken.
        assert_eq!(opted_in("admin"), 5);
    }

    #[test]
    fn webmcp_route_is_not_shadowed_by_a_block_or_flow_of_the_same_name() {
        // `ends_with("/webmcp")` is equally true of `{mount}/webmcp` and of
        // `{mount}/blocks/webmcp`, so ordering the checks decided which one
        // answered. The parent segment settles it instead.
        assert_eq!(super::route_of("/_inspector/webmcp"), super::Route::WebMcp);
        assert_eq!(
            super::route_of("/_inspector/blocks/webmcp"),
            super::Route::Block("webmcp")
        );
        assert_eq!(
            super::route_of("/_inspector/flows/webmcp"),
            super::Route::Flow("webmcp")
        );
        // And the rest of the table still resolves, under any mount.
        assert_eq!(super::route_of("/a/b/c/_inspector/ui"), super::Route::Ui);
        assert_eq!(super::route_of("/_inspector/blocks"), super::Route::Blocks);
        assert_eq!(super::route_of("/_inspector/blocks/"), super::Route::Blocks);
        assert_eq!(
            super::route_of("/_inspector/blocks/org%2Fname"),
            super::Route::Block("org%2Fname")
        );
        assert_eq!(super::route_of("/_inspector"), super::Route::Summary);
        assert_eq!(super::route_of("/"), super::Route::Summary);
    }

    #[tokio::test]
    async fn webmcp_view_shows_a_duplicate_only_in_the_column_it_collides_in() {
        // Tool-name uniqueness is a property of a manifest, so a name claimed
        // by both a public and an admin endpoint is published to the public
        // caller and refused to the admin. The per-level refusal lists are
        // what make that visible; one folded list would hide it.
        let blocks = vec![
            BlockInfo::new("test/shop", "1.0.0", "http-handler@v1", "Shop").endpoints(vec![
                BlockEndpoint::get("/b/shop/thing")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_thing", "Public thing."),
                BlockEndpoint::get("/b/shop/admin/thing")
                    .auth(AuthLevel::Admin)
                    .agent_tool("get_thing", "Admin thing."),
            ]),
        ];

        let body = webmcp_body(blocks).await;
        assert_eq!(names_at(&body, "public"), vec!["get_thing".to_string()]);
        assert_eq!(names_at(&body, "admin"), Vec::<String>::new());
        assert!(
            body["levels"][0]["refusals"]
                .as_array()
                .expect("refusals")
                .is_empty(),
            "nothing collides in the public column: {body}"
        );
        assert_eq!(
            body["levels"][2]["refusals"]
                .as_array()
                .expect("refusals")
                .len(),
            2,
            "both claimants must be named where the collision happens: {body}"
        );
    }

    #[tokio::test]
    async fn webmcp_view_is_behind_the_same_access_policy_as_the_rest() {
        // The columns name every admin-only tool, so the page must not be
        // reachable by a caller the default policy denies.
        let mut msg = Message::new("retrieve");
        msg.set_meta(wafer_block::meta::META_REQ_RESOURCE, "/_inspector/webmcp");
        let out = InspectorBlock::new()
            .handle(
                &BlocksContext { blocks: fixture() },
                msg,
                InputStream::empty(),
            )
            .await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => assert_eq!(e.code, ErrorCode::Unauthenticated),
            other => panic!("the webmcp view must not answer an unauthenticated caller: {other:?}"),
        }
    }
}
