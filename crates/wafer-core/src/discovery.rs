//! Discovery document generation — OpenAPI 3.1 and A2A AgentCard.

use serde_json::{json, Value};
use wafer_block::types::{AuthLevel, BlockEndpoint, BlockInfo, HttpMethod};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn method_key(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "get",
        HttpMethod::Post => "post",
        HttpMethod::Patch => "patch",
        HttpMethod::Delete => "delete",
    }
}

/// Extract properties from a JSON Schema object and turn them into OpenAPI
/// parameter objects with the given `in` value.
fn extract_params(schema: &Value, location: &str) -> Vec<Value> {
    let Some(props) = schema.get("properties").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    props
        .iter()
        .map(|(name, prop_schema)| {
            let is_required = required.contains(&name.as_str());
            json!({
                "name": name,
                "in": location,
                "required": is_required,
                "schema": prop_schema,
            })
        })
        .collect()
}

/// Build a slug from a URL path suitable for use in a skill id.
/// Strips leading `/`, removes braces, and replaces `/` with `_`.
fn path_to_slug(path: &str) -> String {
    path.trim_start_matches('/')
        .replace(['{', '}'], "")
        .replace('/', "_")
}

/// Maximum `$ref` hops to follow before giving up. Self-referential schemas
/// (a `Condition` containing child `Condition`s) are legitimate and would
/// otherwise recurse forever; at the limit we emit `{}` — an unconstrained
/// schema — which is honest about "anything may go here" rather than wrong.
const MAX_REF_DEPTH: u8 = 8;

/// Rewrite a schemars-generated schema into a self-contained one: every
/// `#/$defs/*` reference is replaced by its target, and the `$defs` block is
/// removed.
///
/// OpenAPI clients resolve `$ref` fine, which is why `generate_openapi` does
/// not do this. Many MCP-style clients do not, so the WebMCP projection must
/// hand over schemas that stand alone.
fn inline_refs(schema: &Value) -> Value {
    let defs = schema.get("$defs").cloned().unwrap_or(Value::Null);
    resolve_refs(schema, &defs, 0)
}

fn resolve_refs(node: &Value, defs: &Value, depth: u8) -> Value {
    match node {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get("$ref") {
                if depth >= MAX_REF_DEPTH {
                    return json!({});
                }
                let target = reference
                    .strip_prefix("#/$defs/")
                    .and_then(|name| defs.get(name));
                let mut resolved = match target {
                    Some(found) => resolve_refs(found, defs, depth + 1),
                    None => json!({}),
                };

                // JSON Schema 2020-12 allows keywords ALONGSIDE `$ref`, and
                // schemars uses exactly that: a doc-commented field of a
                // named type emits `{"description": "...", "$ref": "#/$defs/Status"}`.
                // Returning only the resolved target would silently delete
                // every such field description. Siblings win over the
                // target's own keys, since they are the more specific
                // annotation. `$defs` is excluded here too — it is the
                // reference table itself, not a schema keyword, and must
                // never survive into the output (it can appear as a literal
                // sibling of `$ref` when the ref sits at the schema root).
                if let Some(out) = resolved.as_object_mut() {
                    for (key, value) in map {
                        if key == "$ref" || key == "$defs" {
                            continue;
                        }
                        out.insert(key.clone(), resolve_refs(value, defs, depth));
                    }
                }
                return resolved;
            }

            let mut out = serde_json::Map::new();
            for (key, value) in map {
                // `$defs` is the reference table itself, never part of the
                // resulting schema.
                if key == "$defs" {
                    continue;
                }
                out.insert(key.clone(), resolve_refs(value, defs, depth));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| resolve_refs(item, defs, depth))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Collect `properties` and `required` from one schema source into the
/// merged accumulators, returning the property names it contributed.
///
/// Names come back sorted so the generated manifest is byte-stable across
/// runs — `serde_json::Map` iteration order is insertion order, but the
/// upstream schema's order is not something we control.
fn merge_schema_source(
    source: Option<&Value>,
    properties: &mut serde_json::Map<String, Value>,
    required: &mut Vec<String>,
) -> Vec<String> {
    let Some(source) = source else {
        return Vec::new();
    };
    let inlined = inline_refs(source);

    let mut contributed = Vec::new();
    if let Some(props) = inlined.get("properties").and_then(Value::as_object) {
        for (name, schema) in props {
            properties.insert(name.clone(), schema.clone());
            contributed.push(name.clone());
        }
    }
    if let Some(reqs) = inlined.get("required").and_then(Value::as_array) {
        for name in reqs.iter().filter_map(Value::as_str) {
            let owned = name.to_string();
            if !required.contains(&owned) {
                required.push(owned);
            }
        }
    }

    contributed.sort();
    contributed
}

/// The flattened agent-facing input schema for one endpoint, plus the
/// provenance a client needs to rebuild a real HTTP request.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AgentInputSchema {
    pub schema: Value,
    pub path_params: Vec<String>,
    pub query_params: Vec<String>,
    pub body_params: Vec<String>,
    /// Property names contributed by more than one of path/query/body.
    /// Non-empty means this endpoint MUST NOT be exposed as a tool: the
    /// merged schema can only describe one of the colliding locations, so
    /// any tool built from it would misdescribe its own arguments.
    pub collisions: Vec<String>,
}

/// Flatten an endpoint's path, query, and body schemas into the single
/// `inputSchema` a WebMCP tool exposes, plus the provenance the client needs
/// to rebuild a real HTTP request from the agent's flat argument object.
///
/// A property name contributed by more than one of path/query/body is
/// recorded in `collisions` rather than silently resolved by last-source-wins:
/// the merged schema can only describe one of the colliding locations, so a
/// tool built from it would misdescribe its own arguments to the agent. This
/// function does not panic or pick a winner — it reports the conflict and
/// lets the caller (the WebMCP manifest generator) decide to skip the
/// endpoint rather than publish a tool that can lie about its arguments.
fn agent_input_schema(ep: &BlockEndpoint) -> AgentInputSchema {
    let mut properties = serde_json::Map::new();
    let mut required: Vec<String> = Vec::new();

    let path_params = merge_schema_source(ep.path_params.as_ref(), &mut properties, &mut required);
    let query_params =
        merge_schema_source(ep.query_params.as_ref(), &mut properties, &mut required);
    let body_params = merge_schema_source(ep.input_schema.as_ref(), &mut properties, &mut required);

    let mut counts: std::collections::HashMap<&str, u8> = std::collections::HashMap::new();
    for name in path_params
        .iter()
        .chain(query_params.iter())
        .chain(body_params.iter())
    {
        *counts.entry(name.as_str()).or_insert(0) += 1;
    }
    let mut collisions: Vec<String> = counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, _)| name.to_string())
        .collect();
    collisions.sort();

    let mut schema = serde_json::Map::new();
    schema.insert("type".into(), json!("object"));
    schema.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        required.sort();
        schema.insert("required".into(), json!(required));
    }

    AgentInputSchema {
        schema: Value::Object(schema),
        path_params,
        query_params,
        body_params,
        collisions,
    }
}

// ---------------------------------------------------------------------------
// generate_openapi
// ---------------------------------------------------------------------------

/// Generate a full OpenAPI 3.1 JSON document from the given blocks.
pub fn generate_openapi(
    blocks: &[BlockInfo],
    project_name: &str,
    project_description: &str,
    server_url: &str,
) -> Value {
    let mut paths: serde_json::Map<String, Value> = serde_json::Map::new();

    for block in blocks {
        for ep in &block.endpoints {
            if !ep.has_schema() {
                continue;
            }

            let mut operation: serde_json::Map<String, Value> = serde_json::Map::new();

            // summary
            operation.insert("summary".into(), json!(ep.summary));

            // description
            if !ep.description.is_empty() {
                operation.insert("description".into(), json!(ep.description));
            }

            // tags
            if !ep.tags.is_empty() {
                operation.insert("tags".into(), json!(ep.tags));
            }

            // deprecated
            if ep.deprecated {
                operation.insert("deprecated".into(), json!(true));
            }

            // requestBody from input_schema
            if let Some(input) = &ep.input_schema {
                operation.insert(
                    "requestBody".into(),
                    json!({
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": input
                            }
                        }
                    }),
                );
            }

            // parameters from path_params and query_params
            let mut parameters: Vec<Value> = Vec::new();
            if let Some(pp) = &ep.path_params {
                parameters.extend(extract_params(pp, "path"));
            }
            if let Some(qp) = &ep.query_params {
                parameters.extend(extract_params(qp, "query"));
            }
            if !parameters.is_empty() {
                operation.insert("parameters".into(), json!(parameters));
            }

            // responses
            let response_200 = ep.output_schema.as_ref().map_or_else(
                || json!({ "description": "Successful response" }),
                |output| {
                    json!({
                        "description": "Successful response",
                        "content": {
                            "application/json": {
                                "schema": output
                            }
                        }
                    })
                },
            );
            operation.insert("responses".into(), json!({ "200": response_200 }));

            // security
            match ep.auth {
                AuthLevel::Authenticated | AuthLevel::Admin => {
                    operation.insert("security".into(), json!([{ "bearerAuth": [] }]));
                }
                AuthLevel::Public => {
                    // no security field
                }
            }

            let method = method_key(ep.method);
            let path_entry = paths.entry(ep.path.clone()).or_insert_with(|| json!({}));
            path_entry
                .as_object_mut()
                .unwrap()
                .insert(method.into(), Value::Object(operation));
        }
    }

    json!({
        "openapi": "3.1.0",
        "info": {
            "title": project_name,
            "description": project_description,
            "version": "1.0.0"
        },
        "servers": [
            { "url": server_url }
        ],
        "paths": paths,
        "components": {
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    "bearerFormat": "JWT"
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// generate_agent_card
// ---------------------------------------------------------------------------

/// Generate an A2A AgentCard JSON document from the given blocks.
pub fn generate_agent_card(
    blocks: &[BlockInfo],
    project_name: &str,
    project_description: &str,
    server_url: &str,
) -> Value {
    let mut skills: Vec<Value> = Vec::new();

    for block in blocks {
        for ep in &block.endpoints {
            if !ep.has_schema() {
                continue;
            }

            let path_slug = path_to_slug(&ep.path);
            let method = method_key(ep.method);
            let skill_id = format!("{}/{}_{}", block.name, method, path_slug);

            let description = if !ep.description.is_empty() {
                ep.description.clone()
            } else {
                ep.summary.clone()
            };

            let has_input = ep.input_schema.is_some();
            let has_output = ep.output_schema.is_some();

            let input_modes: Vec<&str> = if has_input {
                vec!["application/json"]
            } else {
                vec![]
            };
            let output_modes: Vec<&str> = if has_output {
                vec!["application/json"]
            } else {
                vec![]
            };

            let skill = json!({
                "id": skill_id,
                "name": ep.summary,
                "description": description,
                "tags": ep.tags,
                "input_modes": input_modes,
                "output_modes": output_modes,
            });

            skills.push(skill);
        }
    }

    json!({
        "name": project_name,
        "description": project_description,
        "version": "1.0.0",
        "supported_interfaces": [
            {
                "url": format!("{}/a2a", server_url),
                "protocol_binding": "JSONRPC",
                "protocol_version": "1.0"
            }
        ],
        "capabilities": {
            "streaming": true,
            "pushNotifications": false
        },
        "security_schemes": {
            "bearerAuth": {
                "type": "http",
                "scheme": "bearer",
                "bearerFormat": "JWT"
            }
        },
        "default_input_modes": ["application/json"],
        "default_output_modes": ["application/json"],
        "skills": skills,
    })
}

// ---------------------------------------------------------------------------
// generate_webmcp
// ---------------------------------------------------------------------------

/// `Public < Authenticated < Admin`, expressed explicitly because
/// `AuthLevel` deliberately does not derive `Ord`.
fn auth_rank(level: AuthLevel) -> u8 {
    match level {
        AuthLevel::Public => 0,
        AuthLevel::Authenticated => 1,
        AuthLevel::Admin => 2,
    }
}

/// Project the blocks' endpoint declarations into a WebMCP tool manifest,
/// filtered to what `caller` is allowed to invoke.
///
/// This is the third projection of `BlockInfo::endpoints`, alongside
/// [`generate_openapi`] and [`generate_agent_card`]. Two things make it
/// different from those:
///
/// * **Opt-in.** Only endpoints carrying [`AgentTool`] metadata appear.
///   Carrying a schema is not consent to being called by an agent.
/// * **Auth-filtered.** Tools above `caller`'s level are omitted entirely —
///   not marked unavailable. A name an agent cannot use is recon surface, so
///   it never reaches the page. This mirrors the [SEC-073] posture applied to
///   the discovery documents.
pub fn generate_webmcp(blocks: &[BlockInfo], caller: AuthLevel) -> Value {
    let ceiling = auth_rank(caller);
    let mut tools: Vec<Value> = Vec::new();

    for block in blocks {
        for ep in &block.endpoints {
            let Some(tool) = ep.agent_tool.as_ref() else {
                continue;
            };
            if auth_rank(ep.auth) > ceiling {
                continue;
            }

            let input = agent_input_schema(ep);

            // A property name arriving from two of path/query/body cannot be
            // honestly described by one flat schema, and the client would put
            // the value in both places. Emitting no tool is the safe, visible
            // failure; emitting a lying one is neither.
            if !input.collisions.is_empty() {
                continue;
            }

            tools.push(json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": input.schema,
                "invocation": {
                    "method": method_key(ep.method),
                    "path": ep.path,
                    "path_params": input.path_params,
                    "query_params": input.query_params,
                    "body_params": input.body_params,
                },
            }));
        }
    }

    json!({
        "schema_version": 1,
        "tools": tools,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn test_block() -> BlockInfo {
        BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "A test block")
            .description("Test block for unit tests")
            .endpoints(vec![
                BlockEndpoint::post("/b/test/api/login")
                    .summary("Login")
                    .description("Authenticate with credentials")
                    .auth(AuthLevel::Public)
                    .input_schema(json!({"type": "object", "properties": {"email": {"type": "string"}, "password": {"type": "string"}}, "required": ["email", "password"]}))
                    .output_schema(json!({"type": "object", "properties": {"token": {"type": "string"}}}))
                    .tags(&["auth"]),
                BlockEndpoint::get("/b/test/api/me")
                    .summary("Get current user")
                    .auth(AuthLevel::Authenticated)
                    .output_schema(json!({"type": "object", "properties": {"id": {"type": "string"}, "email": {"type": "string"}}}))
                    .tags(&["auth", "users"]),
                BlockEndpoint::get("/b/test/health")
                    .summary("Health check"),
            ])
    }

    // 1. openapi_basic_structure
    #[test]
    fn openapi_basic_structure() {
        let block = test_block();
        let doc = generate_openapi(
            &[block],
            "My Project",
            "A test project",
            "https://example.com",
        );

        assert_eq!(doc["openapi"], "3.1.0");
        assert_eq!(doc["info"]["title"], "My Project");
        assert_eq!(doc["info"]["description"], "A test project");
        assert_eq!(doc["info"]["version"], "1.0.0");
        assert_eq!(doc["servers"][0]["url"], "https://example.com");
    }

    // 2. openapi_includes_schema_endpoints_only
    #[test]
    fn openapi_includes_schema_endpoints_only() {
        let block = test_block();
        let doc = generate_openapi(&[block], "P", "", "https://x.com");

        // /b/test/health has no schema — should not appear
        assert!(
            doc["paths"].get("/b/test/health").is_none(),
            "health endpoint (no schema) should be excluded"
        );
        // /b/test/api/login has input + output schema
        assert!(
            doc["paths"].get("/b/test/api/login").is_some(),
            "login endpoint should be included"
        );
        // /b/test/api/me has output schema
        assert!(
            doc["paths"].get("/b/test/api/me").is_some(),
            "me endpoint should be included"
        );
    }

    // 3. openapi_post_has_request_body
    #[test]
    fn openapi_post_has_request_body() {
        let block = test_block();
        let doc = generate_openapi(&[block], "P", "", "https://x.com");

        let op = &doc["paths"]["/b/test/api/login"]["post"];
        assert!(
            !op["requestBody"].is_null(),
            "POST with input_schema should have requestBody"
        );
        assert_eq!(
            op["requestBody"]["content"]["application/json"]["schema"]["type"],
            "object"
        );
    }

    // 4. openapi_get_has_response_schema
    #[test]
    fn openapi_get_has_response_schema() {
        let block = test_block();
        let doc = generate_openapi(&[block], "P", "", "https://x.com");

        let op = &doc["paths"]["/b/test/api/me"]["get"];
        let schema = &op["responses"]["200"]["content"]["application/json"]["schema"];
        assert_eq!(schema["type"], "object");
    }

    // 5. openapi_auth_sets_security
    #[test]
    fn openapi_auth_sets_security() {
        let block = test_block();
        let doc = generate_openapi(&[block], "P", "", "https://x.com");

        // Public endpoint: no security field
        let login_op = &doc["paths"]["/b/test/api/login"]["post"];
        assert!(
            login_op.get("security").is_none(),
            "Public endpoint should not have a security field"
        );

        // Authenticated endpoint: bearerAuth
        let me_op = &doc["paths"]["/b/test/api/me"]["get"];
        assert_eq!(
            me_op["security"][0]["bearerAuth"],
            json!([]),
            "Authenticated endpoint should have bearerAuth security"
        );
    }

    // 6. openapi_tags_propagated
    #[test]
    fn openapi_tags_propagated() {
        let block = test_block();
        let doc = generate_openapi(&[block], "P", "", "https://x.com");

        let login_tags = &doc["paths"]["/b/test/api/login"]["post"]["tags"];
        assert_eq!(*login_tags, json!(["auth"]));

        let me_tags = &doc["paths"]["/b/test/api/me"]["get"]["tags"];
        assert_eq!(*me_tags, json!(["auth", "users"]));
    }

    // 7. openapi_security_scheme_present
    #[test]
    fn openapi_security_scheme_present() {
        let block = test_block();
        let doc = generate_openapi(&[block], "P", "", "https://x.com");

        let bearer = &doc["components"]["securitySchemes"]["bearerAuth"];
        assert_eq!(bearer["type"], "http");
        assert_eq!(bearer["scheme"], "bearer");
    }

    // 8. agent_card_basic_structure
    #[test]
    fn agent_card_basic_structure() {
        let block = test_block();
        let card = generate_agent_card(
            &[block],
            "My Project",
            "A test project",
            "https://example.com",
        );

        assert_eq!(card["name"], "My Project");
        assert_eq!(card["description"], "A test project");
        assert_eq!(card["version"], "1.0.0");
        assert_eq!(
            card["supported_interfaces"][0]["url"],
            "https://example.com/a2a"
        );
        assert_eq!(
            card["supported_interfaces"][0]["protocol_binding"],
            "JSONRPC"
        );
    }

    // 9. agent_card_skills_from_schema_endpoints
    #[test]
    fn agent_card_skills_from_schema_endpoints() {
        let block = test_block();
        let card = generate_agent_card(&[block], "P", "", "https://x.com");

        let skills = card["skills"].as_array().unwrap();
        // Only 2 schema endpoints (login + me); health has no schema
        assert_eq!(
            skills.len(),
            2,
            "should have 2 skills (schema endpoints only)"
        );

        let login_skill = skills.iter().find(|s| s["name"] == "Login").unwrap();
        assert_eq!(login_skill["tags"], json!(["auth"]));

        let me_skill = skills
            .iter()
            .find(|s| s["name"] == "Get current user")
            .unwrap();
        assert_eq!(me_skill["tags"], json!(["auth", "users"]));
    }

    // 10. agent_card_skill_ids_include_block_name
    #[test]
    fn agent_card_skill_ids_include_block_name() {
        let block = test_block();
        let card = generate_agent_card(&[block], "P", "", "https://x.com");

        let skills = card["skills"].as_array().unwrap();
        for skill in skills {
            let id = skill["id"].as_str().unwrap();
            assert!(
                id.starts_with("test/block/"),
                "skill id '{id}' should start with block name 'test/block/'"
            );
        }
    }

    // 11. agent_card_capabilities_defaults
    #[test]
    fn agent_card_capabilities_defaults() {
        let block = test_block();
        let card = generate_agent_card(&[block], "P", "", "https://x.com");

        assert_eq!(card["capabilities"]["streaming"], true);
        assert_eq!(card["capabilities"]["pushNotifications"], false);
    }

    // 12. inline_refs_leaves_flat_schema_unchanged
    #[test]
    fn inline_refs_leaves_flat_schema_unchanged() {
        let schema = json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        });
        assert_eq!(inline_refs(&schema), schema);
    }

    // 13. inline_refs_replaces_ref_with_target_and_drops_defs
    #[test]
    fn inline_refs_replaces_ref_with_target_and_drops_defs() {
        let schema = json!({
            "type": "object",
            "properties": { "status": { "$ref": "#/$defs/ProductStatus" } },
            "$defs": {
                "ProductStatus": { "type": "string", "enum": ["draft", "active"] }
            }
        });
        let out = inline_refs(&schema);
        assert_eq!(
            out["properties"]["status"],
            json!({ "type": "string", "enum": ["draft", "active"] })
        );
        assert!(out.get("$defs").is_none(), "$defs must be stripped: {out}");
    }

    // 14. inline_refs_resolves_nested_refs
    #[test]
    fn inline_refs_resolves_nested_refs() {
        let schema = json!({
            "type": "object",
            "properties": { "tier": { "$ref": "#/$defs/PricingTier" } },
            "$defs": {
                "PricingTier": {
                    "type": "object",
                    "properties": { "scheme": { "$ref": "#/$defs/BillingScheme" } }
                },
                "BillingScheme": { "type": "string", "enum": ["per_unit", "tiered"] }
            }
        });
        let out = inline_refs(&schema);
        assert_eq!(
            out["properties"]["tier"]["properties"]["scheme"],
            json!({ "type": "string", "enum": ["per_unit", "tiered"] })
        );
    }

    // 15. inline_refs_resolves_refs_inside_arrays
    #[test]
    fn inline_refs_resolves_refs_inside_arrays() {
        let schema = json!({
            "type": "object",
            "properties": {
                "offers": { "type": "array", "items": { "$ref": "#/$defs/Offer" } }
            },
            "$defs": { "Offer": { "type": "object" } }
        });
        let out = inline_refs(&schema);
        assert_eq!(
            out["properties"]["offers"]["items"],
            json!({ "type": "object" })
        );
    }

    // 16. inline_refs_terminates_on_self_referential_schema
    #[test]
    fn inline_refs_terminates_on_self_referential_schema() {
        // A `Condition` that can contain child `Condition`s is a real shape in
        // products/contracts.rs. Inlining must bottom out rather than recurse
        // forever.
        let schema = json!({
            "$ref": "#/$defs/Condition",
            "$defs": {
                "Condition": {
                    "type": "object",
                    "properties": { "all_of": { "type": "array", "items": { "$ref": "#/$defs/Condition" } } }
                }
            }
        });
        let out = inline_refs(&schema);
        assert_eq!(out["type"], json!("object"));
        let rendered = out.to_string();
        assert!(
            !rendered.contains("$ref"),
            "no unresolved $ref may survive: {rendered}"
        );
    }

    // 17. inline_refs_strips_defs_even_when_ref_is_schema_root
    #[test]
    fn inline_refs_strips_defs_even_when_ref_is_schema_root() {
        // When `$ref` sits at the schema root, `$defs` is a literal sibling
        // of it in the same JSON object — the same shape that carries a
        // legitimate sibling like `description` in the test above. Unlike
        // `description`, `$defs` is the reference table itself and must
        // never be merged back into the output.
        let schema = json!({
            "$ref": "#/$defs/Condition",
            "$defs": {
                "Condition": {
                    "type": "object",
                    "properties": { "all_of": { "type": "array", "items": { "$ref": "#/$defs/Condition" } } }
                }
            }
        });
        let out = inline_refs(&schema);
        assert!(out.get("$defs").is_none(), "$defs must be stripped: {out}");
    }

    // 18. inline_refs_drops_unresolvable_ref_to_empty_schema
    #[test]
    fn inline_refs_drops_unresolvable_ref_to_empty_schema() {
        let schema = json!({ "properties": { "x": { "$ref": "#/$defs/Missing" } } });
        let out = inline_refs(&schema);
        assert_eq!(out["properties"]["x"], json!({}));
    }

    // 19. inline_refs_preserves_keywords_sitting_beside_a_ref
    #[test]
    fn inline_refs_preserves_keywords_sitting_beside_a_ref() {
        // JSON Schema 2020-12 allows keywords alongside `$ref`, and schemars uses
        // that for field-level docs on a named type. Returning only the target
        // would delete every such description.
        let schema = json!({
            "type": "object",
            "properties": {
                "status": {
                    "description": "Current lifecycle status of the product.",
                    "$ref": "#/$defs/ProductStatus"
                }
            },
            "$defs": {
                "ProductStatus": { "type": "string", "enum": ["draft", "active"] }
            }
        });
        let out = inline_refs(&schema);
        let status = &out["properties"]["status"];

        assert_eq!(
            status["description"],
            json!("Current lifecycle status of the product."),
            "a description beside $ref must survive inlining: {status}"
        );
        assert_eq!(status["type"], json!("string"));
        assert_eq!(status["enum"], json!(["draft", "active"]));
    }

    // 20. agent_input_schema_is_empty_object_when_endpoint_has_no_schemas
    #[test]
    fn agent_input_schema_is_empty_object_when_endpoint_has_no_schemas() {
        let ep = BlockEndpoint::get("/b/products/storefront/config");
        let result = agent_input_schema(&ep);
        assert_eq!(result.schema, json!({ "type": "object", "properties": {} }));
        assert!(
            result.path_params.is_empty()
                && result.query_params.is_empty()
                && result.body_params.is_empty()
        );
        assert!(result.collisions.is_empty());
    }

    // 21. agent_input_schema_merges_all_three_sources_and_records_provenance
    #[test]
    fn agent_input_schema_merges_all_three_sources_and_records_provenance() {
        let ep = BlockEndpoint::post("/b/products/products/{product_id}/offers")
            .path_params_schema(json!({
                "type": "object",
                "properties": { "product_id": { "type": "string" } },
                "required": ["product_id"]
            }))
            .query_params_schema(json!({
                "type": "object",
                "properties": { "expand": { "type": "string" } }
            }))
            .input_schema(json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }));

        let result = agent_input_schema(&ep);

        assert_eq!(
            result.schema["properties"]["product_id"],
            json!({ "type": "string" })
        );
        assert_eq!(
            result.schema["properties"]["expand"],
            json!({ "type": "string" })
        );
        assert_eq!(
            result.schema["properties"]["name"],
            json!({ "type": "string" })
        );

        assert_eq!(
            result.schema["required"],
            json!(["name", "product_id"]),
            "required must be exactly the sorted union of both sources' required lists, \
             with expand excluded and no duplicates"
        );

        assert_eq!(result.path_params, vec!["product_id".to_string()]);
        assert_eq!(result.query_params, vec!["expand".to_string()]);
        assert_eq!(result.body_params, vec!["name".to_string()]);
        assert!(result.collisions.is_empty());
    }

    // 22. agent_input_schema_inlines_refs_from_each_source
    #[test]
    fn agent_input_schema_inlines_refs_from_each_source() {
        let ep = BlockEndpoint::post("/b/products/checkout").input_schema(json!({
            "type": "object",
            "properties": { "presentation": { "$ref": "#/$defs/CheckoutPresentation" } },
            "$defs": {
                "CheckoutPresentation": { "type": "string", "enum": ["hosted", "embedded"] }
            }
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(
            result.schema["properties"]["presentation"],
            json!({ "type": "string", "enum": ["hosted", "embedded"] })
        );
        assert!(result.schema.get("$defs").is_none());
        assert_eq!(result.body_params, vec!["presentation".to_string()]);
    }

    // 23. agent_input_schema_omits_required_key_when_nothing_is_required
    #[test]
    fn agent_input_schema_omits_required_key_when_nothing_is_required() {
        let ep = BlockEndpoint::get("/b/products/list").query_params_schema(json!({
            "type": "object",
            "properties": { "page": { "type": "integer" } }
        }));
        let result = agent_input_schema(&ep);
        assert!(
            result.schema.get("required").is_none(),
            "an all-optional schema must not carry an empty required array: {}",
            result.schema
        );
        assert_eq!(result.query_params, vec!["page".to_string()]);
    }

    // 24. agent_input_schema_provenance_is_sorted_for_deterministic_output
    #[test]
    fn agent_input_schema_provenance_is_sorted_for_deterministic_output() {
        let ep = BlockEndpoint::get("/b/x/{b}/{a}").path_params_schema(json!({
            "type": "object",
            "properties": { "b": { "type": "string" }, "a": { "type": "string" } }
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(result.path_params, vec!["a".to_string(), "b".to_string()]);
    }

    // 25. agent_input_schema_no_collision_across_distinct_names
    #[test]
    fn agent_input_schema_no_collision_across_distinct_names() {
        // Same multi-source shape as test 21: three distinct property names,
        // one per source — nothing should be flagged as colliding.
        let ep = BlockEndpoint::post("/b/products/products/{product_id}/offers")
            .path_params_schema(json!({
                "type": "object",
                "properties": { "product_id": { "type": "string" } }
            }))
            .query_params_schema(json!({
                "type": "object",
                "properties": { "expand": { "type": "string" } }
            }))
            .input_schema(json!({
                "type": "object",
                "properties": { "name": { "type": "string" } }
            }));

        let result = agent_input_schema(&ep);
        assert!(result.collisions.is_empty());
    }

    // 26. agent_input_schema_records_collision_between_path_and_body
    #[test]
    fn agent_input_schema_records_collision_between_path_and_body() {
        let ep = BlockEndpoint::post("/b/products/products/{id}")
            .path_params_schema(json!({
                "type": "object",
                "properties": { "id": { "type": "string" } }
            }))
            .input_schema(json!({
                "type": "object",
                "properties": { "id": { "type": "integer" } }
            }));

        let result = agent_input_schema(&ep);
        assert_eq!(
            result.collisions,
            vec!["id".to_string()],
            "a name contributed by both path and body must be flagged, not silently \
             resolved by last-source-wins"
        );
        // Provenance still records the name on both sides — the caller
        // needs that to see exactly which locations collided.
        assert_eq!(result.path_params, vec!["id".to_string()]);
        assert_eq!(result.body_params, vec!["id".to_string()]);
    }

    // 27. agent_input_schema_collects_every_colliding_name_not_just_the_first
    #[test]
    fn agent_input_schema_collects_every_colliding_name_not_just_the_first() {
        // "id" collides between path and body; "owner" collides between
        // path and query. Both must come back, sorted.
        let ep = BlockEndpoint::post("/b/products/products/{id}/{owner}")
            .path_params_schema(json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "owner": { "type": "string" }
                }
            }))
            .query_params_schema(json!({
                "type": "object",
                "properties": { "owner": { "type": "string" } }
            }))
            .input_schema(json!({
                "type": "object",
                "properties": {
                    "id": { "type": "integer" },
                    "name": { "type": "string" }
                }
            }));

        let result = agent_input_schema(&ep);
        assert_eq!(
            result.collisions,
            vec!["id".to_string(), "owner".to_string()],
            "every colliding name must be collected and sorted, not just the first found"
        );
    }

    // -----------------------------------------------------------------
    // generate_webmcp
    // -----------------------------------------------------------------

    /// Two blocks spanning all three auth levels, used by the tests below.
    fn webmcp_fixture_blocks() -> Vec<BlockInfo> {
        let products = BlockInfo::new(
            "impresspress/products",
            "1.0.0",
            "http-handler@v1",
            "Commerce block",
        )
        .endpoints(vec![
            BlockEndpoint::get("/b/products/storefront/{product_id}")
                .summary("Storefront product")
                .auth(AuthLevel::Public)
                .path_params_schema(json!({
                    "type": "object",
                    "properties": { "product_id": { "type": "string" } },
                    "required": ["product_id"]
                }))
                .agent_tool("get_product", "Fetch a product and its offers by id."),
            BlockEndpoint::get("/b/products/purchases")
                .summary("List own purchases")
                .auth(AuthLevel::Authenticated)
                .output_schema(json!({ "type": "object" }))
                .agent_tool("list_my_purchases", "List the signed-in user's purchases."),
            // Carries schemas but never opted in — must never appear.
            BlockEndpoint::post("/b/products/webhooks")
                .summary("Stripe webhook")
                .auth(AuthLevel::Public)
                .input_schema(json!({ "type": "object" })),
        ]);

        let admin = BlockInfo::new(
            "impresspress/admin",
            "1.0.0",
            "http-handler@v1",
            "Admin block",
        )
        .endpoints(vec![BlockEndpoint::get("/b/admin/api/users")
            .summary("List users")
            .auth(AuthLevel::Admin)
            .output_schema(json!({ "type": "object" }))
            .agent_tool("list_users", "List all user accounts.")]);

        vec![products, admin]
    }

    fn tool_names(doc: &Value) -> Vec<String> {
        doc["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|t| t["name"].as_str().expect("tool name").to_string())
            .collect()
    }

    #[test]
    fn webmcp_public_caller_sees_only_public_tools() {
        let doc = generate_webmcp(&webmcp_fixture_blocks(), AuthLevel::Public);
        assert_eq!(tool_names(&doc), vec!["get_product".to_string()]);
    }

    #[test]
    fn webmcp_authenticated_caller_sees_public_and_authenticated() {
        let doc = generate_webmcp(&webmcp_fixture_blocks(), AuthLevel::Authenticated);
        let names = tool_names(&doc);
        assert!(names.contains(&"get_product".to_string()));
        assert!(names.contains(&"list_my_purchases".to_string()));
        assert!(
            !names.contains(&"list_users".to_string()),
            "admin tool must not leak to an authenticated caller: {names:?}"
        );
    }

    #[test]
    fn webmcp_admin_caller_sees_every_tool() {
        let doc = generate_webmcp(&webmcp_fixture_blocks(), AuthLevel::Admin);
        assert_eq!(tool_names(&doc).len(), 3);
    }

    #[test]
    fn webmcp_excludes_endpoints_that_did_not_opt_in() {
        let doc = generate_webmcp(&webmcp_fixture_blocks(), AuthLevel::Admin);
        let rendered = doc.to_string();
        assert!(
            !rendered.contains("/b/products/webhooks"),
            "a schema-carrying endpoint without agent_tool must be absent: {rendered}"
        );
    }

    #[test]
    fn webmcp_skips_an_endpoint_whose_parameter_names_collide() {
        // `id` arrives from BOTH the path and the body. One flat schema cannot
        // honestly describe both locations, so no tool may be emitted — an
        // absent tool is visible, a lying one is not.
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/x/{id}")
                    .summary("Collides")
                    .auth(AuthLevel::Public)
                    .path_params_schema(json!({
                        "type": "object",
                        "properties": { "id": { "type": "string" } },
                        "required": ["id"]
                    }))
                    .input_schema(json!({
                        "type": "object",
                        "properties": { "id": { "type": "integer" } }
                    }))
                    .agent_tool("colliding_tool", "Should never be emitted."),
            ]);

        let doc = generate_webmcp(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "an endpoint with a cross-location name collision must produce no tool: {doc}"
        );
    }

    #[test]
    fn webmcp_tool_carries_invocation_metadata() {
        let doc = generate_webmcp(&webmcp_fixture_blocks(), AuthLevel::Public);
        let tool = &doc["tools"][0];
        assert_eq!(tool["name"], json!("get_product"));
        assert_eq!(
            tool["description"],
            json!("Fetch a product and its offers by id.")
        );
        assert_eq!(tool["invocation"]["method"], json!("get"));
        assert_eq!(
            tool["invocation"]["path"],
            json!("/b/products/storefront/{product_id}")
        );
        assert_eq!(tool["invocation"]["path_params"], json!(["product_id"]));
        assert_eq!(tool["invocation"]["query_params"], json!([]));
        assert_eq!(tool["invocation"]["body_params"], json!([]));
        assert_eq!(
            tool["inputSchema"]["properties"]["product_id"],
            json!({ "type": "string" })
        );
    }

    #[test]
    fn webmcp_emits_schema_version_and_empty_tools_for_no_blocks() {
        let doc = generate_webmcp(&[], AuthLevel::Admin);
        assert_eq!(doc["schema_version"], json!(1));
        assert_eq!(doc["tools"], json!([]));
    }

    #[test]
    fn webmcp_tool_order_is_deterministic() {
        let blocks = webmcp_fixture_blocks();
        let first = generate_webmcp(&blocks, AuthLevel::Admin);
        let second = generate_webmcp(&blocks, AuthLevel::Admin);
        assert_eq!(first, second);
    }
}
