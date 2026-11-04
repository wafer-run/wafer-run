//! The `{mount}/webmcp` view: the WebMCP tool manifest as each auth level
//! receives it. The inspector's only page whose content is a security claim.

use std::sync::Arc;

use wafer_block::{AuthLevel, BlockInfo};

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
pub(crate) fn webmcp_view(blocks: &[Arc<BlockInfo>]) -> serde_json::Value {
    use wafer_core::discovery::WebMcpRefusalScope;

    let levels: Vec<serde_json::Value> = WEBMCP_LEVELS
        .iter()
        .map(|level| {
            let (manifest, refusals) =
                wafer_core::discovery::generate_webmcp_report(blocks, *level, |_block, ep| {
                    Some(ep.auth)
                });
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

#[cfg(test)]
mod tests {
    //! These pin the claim rather than the markup — that the three columns
    //! really differ, that an endpoint which opted in and produced no tool is
    //! named with a reason, and that the route does not need the runtime's
    //! flow capability to answer.

    use std::sync::Arc;

    use wafer_block::{AuthLevel, BlockEndpoint, BlockInfo, ErrorCode};

    use crate::test_support::{admin, get, BlocksContext};

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
        let body = get(
            &BlocksContext {
                blocks: blocks.into_iter().map(Arc::new).collect(),
                ..Default::default()
            },
            "/_inspector/webmcp",
            &admin(),
        )
        .await
        .unwrap_or_else(|code| panic!("unexpected error: {code:?}"));
        serde_json::from_slice(&body).expect("valid JSON body")
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
        let denied = get(
            &BlocksContext {
                blocks: fixture().into_iter().map(Arc::new).collect(),
                ..Default::default()
            },
            "/_inspector/webmcp",
            &[],
        )
        .await;
        assert_eq!(
            denied.map(|_| ()),
            Err(ErrorCode::Unauthenticated),
            "the webmcp view must not answer an unauthenticated caller"
        );
    }
}
