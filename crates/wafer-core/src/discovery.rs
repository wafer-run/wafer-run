//! Discovery document generation — OpenAPI 3.1 and A2A AgentCard.

use serde_json::{json, Value};
#[cfg(test)]
use wafer_block::types::BlockEndpoint;
use wafer_block::types::{AuthLevel, BlockInfo, HttpMethod};

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
// `inline_refs` has no caller yet outside `#[cfg(test)]` — it lands in this
// task standalone and is wired into the WebMCP schema projection in a
// follow-up task. Allow dead_code until then rather than gating the
// production function itself behind `#[cfg(test)]`.
#[allow(dead_code)]
const MAX_REF_DEPTH: u8 = 8;

/// Rewrite a schemars-generated schema into a self-contained one: every
/// `#/$defs/*` reference is replaced by its target, and the `$defs` block is
/// removed.
///
/// OpenAPI clients resolve `$ref` fine, which is why `generate_openapi` does
/// not do this. Many MCP-style clients do not, so the WebMCP projection must
/// hand over schemas that stand alone.
#[allow(dead_code)]
fn inline_refs(schema: &Value) -> Value {
    let defs = schema.get("$defs").cloned().unwrap_or(Value::Null);
    resolve_refs(schema, &defs, 0)
}

#[allow(dead_code)]
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
}
