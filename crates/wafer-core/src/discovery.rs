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
/// (a `Condition` containing child `Condition`s) are legitimate and have no
/// finite inlining; at the limit the chain is truncated to `{}` and reported
/// as unresolved, because a truncated chain describes "anything may go here"
/// where the server requires a specific shape.
const MAX_REF_DEPTH: u8 = 8;

/// Decode a `#/$defs/` pointer segment back into the key it names in the
/// `$defs` table.
///
/// schemars writes reference *names* through its `encode_ref_name`: `~`
/// becomes `~0` and `/` becomes `~1` (RFC 6901 JSON-Pointer escaping), and
/// every other byte outside the URI-fragment safe set is percent-encoded
/// (space, `"`, `#`, `%`, `<`, `>`, `[`, `\`, `]`, `^`, `` ` ``, `{`, `|`,
/// `}`, and anything non-ASCII). The `$defs` *keys* are left unencoded, so
/// `#[schemars(rename = "Product Status")]` emits a reference to
/// `#/$defs/Product%20Status` against a table keyed `Product Status`.
/// Looking the raw segment up would miss and silently degrade the property
/// to `{}`.
///
/// Unescaping order is load-bearing, and is the order RFC 6901 §4 requires:
/// `~1` first, then `~0`. Doing `~0` first would turn the encoding of the
/// literal name `~1` (which is `~01`) into `~1` and then into `/`.
///
/// Returns `None` when percent-decoding does not yield valid UTF-8 — a
/// segment that cannot name any key, and so must be reported as unresolvable
/// rather than guessed at.
fn decode_ref_name(encoded: &str) -> Option<String> {
    let decoded = percent_encoding::percent_decode_str(encoded)
        .decode_utf8()
        .ok()?;
    Some(decoded.replace("~1", "/").replace("~0", "~"))
}

/// Rewrite a schemars-generated schema into a self-contained one: every
/// `#/$defs/*` reference is replaced by its target, and the `$defs` block is
/// removed.
///
/// OpenAPI clients resolve `$ref` fine, which is why `generate_openapi` does
/// not do this. Many MCP-style clients do not, so the WebMCP projection must
/// hand over schemas that stand alone.
///
/// Returns the rewritten schema together with a flag saying whether *any*
/// reference in it, at any depth, failed to resolve — no matching `$defs`
/// entry, a form this function does not understand (schemars' root-recursion
/// marker `{"$ref": "#"}`), or a chain that ran past [`MAX_REF_DEPTH`]. All
/// three leave `{}` behind: an unconstrained schema standing where the
/// server requires a concrete type. At the top level that shows up as a
/// missing `properties` object and is caught by the `unrepresentable` check,
/// but below the top level — `properties.children.items` for a
/// `struct Node { children: Vec<Node> }` — nothing else would notice. So the
/// flag travels out of here and callers refuse to build a tool from a schema
/// that sets it.
fn inline_refs(schema: &Value) -> (Value, bool) {
    let defs = schema.get("$defs").cloned().unwrap_or(Value::Null);
    let mut unresolved = false;
    let resolved = resolve_refs(schema, &defs, 0, &mut unresolved);
    (resolved, unresolved)
}

fn resolve_refs(node: &Value, defs: &Value, depth: u8, unresolved: &mut bool) -> Value {
    match node {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get("$ref") {
                if depth >= MAX_REF_DEPTH {
                    *unresolved = true;
                    return json!({});
                }
                let target = reference
                    .strip_prefix("#/$defs/")
                    .and_then(decode_ref_name)
                    .and_then(|name| defs.get(name));
                let mut resolved = match target {
                    Some(found) => resolve_refs(found, defs, depth + 1, unresolved),
                    None => {
                        *unresolved = true;
                        json!({})
                    }
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
                        out.insert(key.clone(), resolve_refs(value, defs, depth, unresolved));
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
                out.insert(key.clone(), resolve_refs(value, defs, depth, unresolved));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| resolve_refs(item, defs, depth, unresolved))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// What one schema source contributed to the merged agent input schema, and
/// the two ways it can be unusable.
#[derive(Debug, Default)]
struct MergedSource {
    /// Top-level property names this source contributed, sorted.
    contributed: Vec<String>,
    /// The source is present and non-empty but its inlined form has no
    /// top-level `properties` object — see [`merge_schema_source`].
    unrepresentable: bool,
    /// Inlining hit a `$ref` it could not resolve, at any depth — see
    /// [`inline_refs`].
    unresolved_ref: bool,
}

/// Collect `properties` and `required` from one schema source into the
/// merged accumulators, reporting the property names it contributed and
/// whether the source could be represented as a flat object schema at all.
///
/// Names come back sorted so the generated manifest is byte-stable across
/// runs — `serde_json::Map`'s default backing is already a `BTreeMap`
/// (`serde_json`'s `preserve_order` feature, which would make it
/// insertion-ordered instead, is not enabled anywhere in this workspace),
/// but the upstream schema's own key order is not something we control, so
/// the sort here makes that intent explicit and keeps it correct if that
/// feature is ever flipped on.
///
/// A source is *unrepresentable* when it is present and non-empty but its
/// inlined form has no top-level `properties` object: a tagged-enum body
/// (`#[serde(tag = "...")]` → `{"oneOf": [...]}`), an array body
/// (`Vec<T>` → `{"type": "array", ...}`), or a root-recursive body (schemars
/// closes a cycle on the root type with a bare `{"$ref": "#"}` and no
/// `$defs` table at all — `inline_refs` only resolves `#/$defs/*` pointers,
/// so this survives unresolved and collapses to `{}`). Silently contributing
/// nothing for any of these would make the merged schema claim the source
/// takes no arguments while the server still requires one.
///
/// That check is top-level only, by construction — it asks whether
/// `properties` exists. An unresolvable `$ref` *below* the top level leaves
/// the top level intact and hides a `{}` inside one property, so
/// [`inline_refs`]'s own report is carried out separately in
/// `unresolved_ref` rather than folded into `unrepresentable`.
fn merge_schema_source(
    source: Option<&Value>,
    properties: &mut serde_json::Map<String, Value>,
    required: &mut Vec<String>,
) -> MergedSource {
    let Some(source) = source else {
        return MergedSource::default();
    };
    let (inlined, unresolved_ref) = inline_refs(source);

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

    let is_present_and_nonempty = match source {
        Value::Null => false,
        Value::Object(map) => !map.is_empty(),
        _ => true,
    };
    let has_properties = inlined
        .get("properties")
        .and_then(Value::as_object)
        .is_some();
    let unrepresentable = is_present_and_nonempty && !has_properties;

    MergedSource {
        contributed,
        unrepresentable,
        unresolved_ref,
    }
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
    /// Source labels (`"path"`, `"query"`, `"body"`) that were present and
    /// non-empty but contributed no top-level `properties` object — see
    /// [`merge_schema_source`] for the shapes that trigger this. Non-empty
    /// means this endpoint MUST NOT be exposed as a tool: the manifest would
    /// claim the tool takes no arguments from that source while the server
    /// still requires one, and a tool that can lie about its own arguments
    /// is worse than no tool.
    pub unrepresentable: Vec<String>,
    /// Source labels (`"path"`, `"query"`, `"body"`) whose inlining hit a
    /// `$ref` it could not resolve — see [`inline_refs`]. Non-empty means
    /// this endpoint MUST NOT be exposed as a tool: somewhere in the schema
    /// sits a bare `{}` that accepts anything while the server requires a
    /// specific type. Unlike [`Self::unrepresentable`] this catches the case
    /// at any depth, including the nested one that leaves the top-level
    /// `properties` object looking perfectly healthy.
    pub unresolved_refs: Vec<String>,
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
/// Likewise, a source that cannot be flattened at all (see
/// [`merge_schema_source`]) is recorded in `unrepresentable` rather than
/// silently omitted.
///
/// The merge only reads `properties` and `required` from each source, so a
/// `deny_unknown_fields` source's `additionalProperties: false` is dropped:
/// the merged schema is strictly more permissive than what the server will
/// actually accept.
fn agent_input_schema(ep: &BlockEndpoint) -> AgentInputSchema {
    let mut properties = serde_json::Map::new();
    let mut required: Vec<String> = Vec::new();

    let path = merge_schema_source(ep.path_params.as_ref(), &mut properties, &mut required);
    let query = merge_schema_source(ep.query_params.as_ref(), &mut properties, &mut required);
    let body = merge_schema_source(ep.input_schema.as_ref(), &mut properties, &mut required);

    let mut unrepresentable = Vec::new();
    let mut unresolved_refs = Vec::new();
    for (label, merged) in [("path", &path), ("query", &query), ("body", &body)] {
        if merged.unrepresentable {
            unrepresentable.push(label.to_string());
        }
        if merged.unresolved_ref {
            unresolved_refs.push(label.to_string());
        }
    }
    unrepresentable.sort();
    unresolved_refs.sort();

    let (path_params, query_params, body_params) =
        (path.contributed, query.contributed, body.contributed);

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
        unrepresentable,
        unresolved_refs,
    }
}

/// Extract the `{name}` placeholders from a URL path template, in order of
/// appearance.
///
/// `None` means the template is malformed — an unmatched `{` or `}`, an
/// empty `{}`, or a nested `{`. That is itself grounds to refuse: a path
/// whose placeholders cannot be parsed cannot be filled in reliably either.
fn path_placeholders(path: &str) -> Option<Vec<String>> {
    let mut names = Vec::new();
    let mut rest = path;
    loop {
        let Some(open) = rest.find('{') else {
            // A `}` with no `{` before it anywhere in the remainder.
            return if rest.contains('}') {
                None
            } else {
                Some(names)
            };
        };
        if rest[..open].contains('}') {
            return None;
        }
        let after = &rest[open + 1..];
        let close = after.find('}')?;
        let name = &after[..close];
        if name.is_empty() || name.contains('{') {
            return None;
        }
        names.push(name.to_string());
        rest = &after[close + 1..];
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
/// * **Opt-in.** Only endpoints carrying `AgentTool` metadata appear.
///   Carrying a schema is not consent to being called by an agent.
/// * **Auth-filtered.** Tools above `caller`'s level are omitted entirely —
///   not marked unavailable. A name an agent cannot use is recon surface, so
///   it never reaches the page. This mirrors the SEC-073 posture applied to
///   the discovery documents.
///
/// # Declared auth is not always the enforced auth
///
/// This wrapper treats each endpoint's declared `ep.auth` as the level that
/// will actually be enforced. That is only true when nothing in the
/// consumer's routing can raise it. A consumer that mounts blocks under
/// access-tiered prefixes enforces `max(prefix_tier, ep.auth)`, so a `Public`
/// endpoint mounted under an admin-only prefix would be advertised here to
/// anonymous callers and then rejected on every call — and, in the other
/// direction, a stricter `ep.auth` on a route the consumer serves publicly
/// would hide a tool that is genuinely reachable.
///
/// Only the consumer knows its prefix table, so it must supply the answer:
/// use [`generate_webmcp_with`] and pass a resolver that returns the level
/// the router will really enforce.
pub fn generate_webmcp(blocks: &[BlockInfo], caller: AuthLevel) -> Value {
    generate_webmcp_with(blocks, caller, |_block, ep| ep.auth)
}

/// [`generate_webmcp`], with the effective auth level of each endpoint
/// supplied by the caller.
///
/// `effective_auth` is asked, for one block and one of its endpoints, what
/// access level the consumer's router will actually enforce on that route —
/// which for a prefix-tiered router is `max(prefix_tier, ep.auth)`, not
/// `ep.auth` alone. Everything else matches [`generate_webmcp`], whose docs
/// describe the projection and why the auth filter matters.
pub fn generate_webmcp_with(
    blocks: &[BlockInfo],
    caller: AuthLevel,
    effective_auth: impl Fn(&BlockInfo, &BlockEndpoint) -> AuthLevel,
) -> Value {
    let ceiling = auth_rank(caller);

    // A WebMCP client registers tools by name, so two endpoints sharing a
    // name are ambiguous no matter which one "wins".
    //
    // This pass deliberately runs over EVERY opted-in endpoint — before the
    // auth filter and before every skip below — so a name is unique, or not,
    // for all callers alike. Counting after filtering would make the name's
    // meaning auth-dependent in exactly the way the rule exists to prevent
    // (a public caller gets `get_thing`; an admin sees two and gets
    // neither, so privilege strictly *reduces* the tool set), and would let
    // one filter launder a duplicate for another: drop the colliding
    // `get_thing` first and the surviving one silently inherits the name as
    // if it had always been unique.
    //
    // Counting first and then emitting only what turned out unique is also
    // order-independent by construction, unlike dropping the later
    // duplicate.
    let mut name_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for block in blocks {
        for ep in &block.endpoints {
            if let Some(tool) = ep.agent_tool.as_ref() {
                *name_counts.entry(tool.name.as_str()).or_insert(0) += 1;
            }
        }
    }

    let mut tools: Vec<Value> = Vec::new();

    for block in blocks {
        for ep in &block.endpoints {
            let Some(tool) = ep.agent_tool.as_ref() else {
                continue;
            };
            if name_counts.get(tool.name.as_str()).copied().unwrap_or(0) != 1 {
                continue;
            }
            if auth_rank(effective_auth(block, ep)) > ceiling {
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

            // A source that is present but reduces to no top-level
            // `properties` (a tagged-enum body, an array body, a
            // root-recursive `$ref`) can't be flattened into the
            // object-shaped `inputSchema` a tool exposes. A tool claiming it
            // takes no arguments from that source while the server still
            // requires one is exactly the kind of lie `collisions` above
            // already refuses to tell.
            if !input.unrepresentable.is_empty() {
                continue;
            }

            // A `$ref` that did not resolve leaves `{}` — "send anything" —
            // where the server requires a specific type. Below the top level
            // that is invisible to the check above: the schema still has its
            // `properties` object, one entry of which now accepts garbage.
            // Same lie, quieter.
            if !input.unresolved_refs.is_empty() {
                continue;
            }

            // `invocation.path` is handed to the client verbatim, so every
            // `{placeholder}` in it must have a declared path param to fill
            // it, and every declared path param must have a placeholder to
            // go into. An unfilled placeholder means the client GETs a
            // literal `{product_id}` and 404s forever; a declared param with
            // nowhere to go means an argument the agent supplies is silently
            // dropped. A tool whose URL can never be built is a lie about
            // what it does, so it is not published at all.
            let Some(mut placeholders) = path_placeholders(&ep.path) else {
                continue;
            };
            placeholders.sort();
            placeholders.dedup();
            if placeholders != input.path_params {
                continue;
            }

            // A deprecated endpoint still works, so it is still published —
            // a missing tool helps no one — but the signal travels with it.
            // The machine-readable `deprecated` flag is what a client should
            // key off; the description prefix is what actually reaches a
            // model, since clients routinely forward only name, description,
            // and inputSchema.
            let description = if ep.deprecated {
                format!("[Deprecated] {}", tool.description)
            } else {
                tool.description.clone()
            };

            let mut emitted = serde_json::Map::new();
            emitted.insert("name".into(), json!(tool.name));
            emitted.insert("description".into(), json!(description));
            if ep.deprecated {
                emitted.insert("deprecated".into(), json!(true));
            }
            emitted.insert("inputSchema".into(), input.schema);
            emitted.insert(
                "invocation".into(),
                json!({
                    "method": method_key(ep.method),
                    "path": ep.path,
                    "path_params": input.path_params,
                    "query_params": input.query_params,
                    "body_params": input.body_params,
                }),
            );
            tools.push(Value::Object(emitted));
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
        let (out, unresolved) = inline_refs(&schema);
        assert_eq!(out, schema);
        assert!(!unresolved, "a schema with no $ref resolves cleanly");
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
        let (out, unresolved) = inline_refs(&schema);
        assert_eq!(
            out["properties"]["status"],
            json!({ "type": "string", "enum": ["draft", "active"] })
        );
        assert!(out.get("$defs").is_none(), "$defs must be stripped: {out}");
        assert!(!unresolved, "a resolvable ref must not be reported: {out}");
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
        let (out, unresolved) = inline_refs(&schema);
        assert_eq!(
            out["properties"]["tier"]["properties"]["scheme"],
            json!({ "type": "string", "enum": ["per_unit", "tiered"] })
        );
        assert!(!unresolved, "both hops resolve: {out}");
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
        let (out, unresolved) = inline_refs(&schema);
        assert_eq!(
            out["properties"]["offers"]["items"],
            json!({ "type": "object" })
        );
        assert!(!unresolved, "a ref inside an array resolves: {out}");
    }

    // 16. inline_refs_terminates_on_self_referential_schema
    #[test]
    fn inline_refs_terminates_on_self_referential_schema() {
        // A `Condition` that can contain child `Condition`s is a real shape in
        // products/contracts.rs. Inlining must bottom out rather than recurse
        // forever — and must say so: the chain is cut at MAX_REF_DEPTH and the
        // deepest `items` becomes `{}`, which accepts anything where the
        // server requires a `Condition`.
        let schema = json!({
            "$ref": "#/$defs/Condition",
            "$defs": {
                "Condition": {
                    "type": "object",
                    "properties": { "all_of": { "type": "array", "items": { "$ref": "#/$defs/Condition" } } }
                }
            }
        });
        let (out, unresolved) = inline_refs(&schema);
        assert_eq!(out["type"], json!("object"));
        let rendered = out.to_string();
        assert!(
            !rendered.contains("$ref"),
            "no unresolved $ref may survive: {rendered}"
        );
        assert!(
            unresolved,
            "a chain truncated at MAX_REF_DEPTH leaves `{{}}` behind and must be \
             reported as unresolved: {rendered}"
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
        let (out, _unresolved) = inline_refs(&schema);
        assert!(out.get("$defs").is_none(), "$defs must be stripped: {out}");
    }

    // 18. inline_refs_reports_an_unresolvable_ref_it_had_to_drop
    #[test]
    fn inline_refs_reports_an_unresolvable_ref_it_had_to_drop() {
        let schema = json!({ "properties": { "x": { "$ref": "#/$defs/Missing" } } });
        let (out, unresolved) = inline_refs(&schema);
        assert_eq!(out["properties"]["x"], json!({}));
        assert!(
            unresolved,
            "a ref with no matching $defs entry degrades to `{{}}` and must be \
             reported, not passed off as a schema: {out}"
        );
    }

    // 18b. inline_refs_resolves_a_percent_encoded_ref_name
    #[test]
    fn inline_refs_resolves_a_percent_encoded_ref_name() {
        // schemars percent-encodes reference *names* (`encode_ref_name`) but
        // leaves the `$defs` keys unencoded, so a `#[schemars(rename = "...")]`
        // with a space in it only resolves if the pointer is decoded first.
        let schema = json!({
            "type": "object",
            "properties": { "status": { "$ref": "#/$defs/Product%20Status" } },
            "$defs": {
                "Product Status": { "type": "string", "enum": ["draft", "active"] }
            }
        });
        let (out, unresolved) = inline_refs(&schema);
        assert_eq!(
            out["properties"]["status"],
            json!({ "type": "string", "enum": ["draft", "active"] }),
            "a percent-encoded ref name must be decoded before lookup: {out}"
        );
        assert!(
            !unresolved,
            "the ref resolved, so nothing is reported: {out}"
        );
    }

    // 18c. inline_refs_resolves_a_json_pointer_escaped_ref_name
    #[test]
    fn inline_refs_resolves_a_json_pointer_escaped_ref_name() {
        // `/` -> `~1` and `~` -> `~0`, unescaped in that order per RFC 6901 §4.
        let schema = json!({
            "type": "object",
            "properties": {
                "slash": { "$ref": "#/$defs/a~1b" },
                "tilde": { "$ref": "#/$defs/c~0d" },
                "literal_tilde_one": { "$ref": "#/$defs/~01" }
            },
            "$defs": {
                "a/b": { "type": "string" },
                "c~d": { "type": "integer" },
                "~1": { "type": "boolean" }
            }
        });
        let (out, unresolved) = inline_refs(&schema);
        assert_eq!(out["properties"]["slash"], json!({ "type": "string" }));
        assert_eq!(out["properties"]["tilde"], json!({ "type": "integer" }));
        assert_eq!(
            out["properties"]["literal_tilde_one"],
            json!({ "type": "boolean" }),
            "unescaping `~0` before `~1` would turn `~01` into `/`: {out}"
        );
        assert!(!unresolved, "all three refs resolved: {out}");
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
        let (out, unresolved) = inline_refs(&schema);
        assert!(!unresolved, "the ref resolves: {out}");
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

    // 28. agent_input_schema_flags_tagged_enum_body_as_unrepresentable
    #[test]
    fn agent_input_schema_flags_tagged_enum_body_as_unrepresentable() {
        // A `#[serde(tag = "...")]` enum body has no top-level `properties`
        // object at all — schemars renders it as `oneOf`. The merge must
        // not silently contribute nothing; it must flag the body as
        // unrepresentable so the caller refuses to build a tool from it.
        let ep = BlockEndpoint::post("/b/products/conditions").input_schema(json!({
            "oneOf": [
                { "type": "object", "properties": { "kind": { "const": "always" } } },
                { "type": "object", "properties": { "kind": { "const": "never" } } }
            ]
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(result.unrepresentable, vec!["body".to_string()]);
        assert!(result.body_params.is_empty());
    }

    // 29. agent_input_schema_flags_array_body_as_unrepresentable
    #[test]
    fn agent_input_schema_flags_array_body_as_unrepresentable() {
        let ep = BlockEndpoint::post("/b/products/bulk").input_schema(json!({
            "type": "array",
            "items": { "type": "string" }
        }));
        let result = agent_input_schema(&ep);
        assert_eq!(result.unrepresentable, vec!["body".to_string()]);
        assert!(result.body_params.is_empty());
    }

    // 30. agent_input_schema_flags_root_recursive_ref_as_unrepresentable
    #[test]
    fn agent_input_schema_flags_root_recursive_ref_as_unrepresentable() {
        // schemars closes a cycle on the root type with a bare `{"$ref": "#"}`
        // and no `$defs` table at all. `inline_refs` only resolves
        // `#/$defs/*` pointers, so this survives inlining unresolved and
        // collapses to `{}` — no top-level `properties`.
        let ep = BlockEndpoint::post("/b/products/condition").input_schema(json!({ "$ref": "#" }));
        let result = agent_input_schema(&ep);
        assert_eq!(result.unrepresentable, vec!["body".to_string()]);
        assert!(result.body_params.is_empty());
    }

    // 31. agent_input_schema_ordinary_object_source_is_representable
    #[test]
    fn agent_input_schema_ordinary_object_source_is_representable() {
        let ep = BlockEndpoint::post("/b/products/rename").input_schema(json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        }));
        let result = agent_input_schema(&ep);
        assert!(
            result.unrepresentable.is_empty(),
            "an ordinary object source must not be flagged: {:?}",
            result.unrepresentable
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

        // Name-list assertions above only prove the higher-privilege tools'
        // *names* are absent. Assert on the fully rendered document too, so a
        // future bug that leaks a restricted tool's description or
        // invocation path while omitting only its name would still be
        // caught — auth filtering is the security property this function
        // exists for, and a name an agent cannot use is recon surface.
        let rendered = doc.to_string();
        assert!(
            !rendered.contains("list_my_purchases") && !rendered.contains("/b/products/purchases"),
            "an authenticated-only endpoint must not appear in an anonymous caller's manifest: {rendered}"
        );
        assert!(
            !rendered.contains("List the signed-in user's purchases."),
            "an authenticated-only tool's description must not leak into an anonymous caller's manifest: {rendered}"
        );
        assert!(
            !rendered.contains("list_users") && !rendered.contains("/b/admin/api/users"),
            "an admin-only endpoint must not appear in an anonymous caller's manifest: {rendered}"
        );
        assert!(
            !rendered.contains("List all user accounts."),
            "an admin-only tool's description must not leak into an anonymous caller's manifest: {rendered}"
        );
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

        // As above: the name-list check only proves the admin tool's *name*
        // is absent. Check the full rendered document too, so a leak of the
        // admin tool's description or invocation path (with the name
        // suppressed) would still fail this test.
        let rendered = doc.to_string();
        assert!(
            !rendered.contains("list_users") && !rendered.contains("/b/admin/api/users"),
            "an admin-only endpoint must not appear in an authenticated (non-admin) caller's manifest: {rendered}"
        );
        assert!(
            !rendered.contains("List all user accounts."),
            "an admin-only tool's description must not leak into an authenticated (non-admin) caller's manifest: {rendered}"
        );
    }

    #[test]
    fn webmcp_admin_caller_sees_every_tool() {
        let doc = generate_webmcp(&webmcp_fixture_blocks(), AuthLevel::Admin);
        let mut names = tool_names(&doc);
        names.sort();
        assert_eq!(
            names,
            vec![
                "get_product".to_string(),
                "list_my_purchases".to_string(),
                "list_users".to_string(),
            ],
            "an admin caller must see exactly these three tools, by name — a length-only \
             assertion here previously let real bugs through: {doc}"
        );
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
    fn webmcp_skips_an_endpoint_whose_body_is_a_tagged_enum() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/x/conditions")
                    .summary("Set condition")
                    .auth(AuthLevel::Public)
                    .input_schema(json!({
                        "oneOf": [
                            { "type": "object", "properties": { "kind": { "const": "always" } } }
                        ]
                    }))
                    .agent_tool(
                        "set_condition",
                        "Should never be emitted: body is a tagged enum.",
                    ),
            ]);

        let doc = generate_webmcp(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "a tagged-enum body must produce no tool: {doc}"
        );
    }

    #[test]
    fn webmcp_skips_an_endpoint_whose_body_is_an_array() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/x/bulk")
                    .summary("Bulk import")
                    .auth(AuthLevel::Public)
                    .input_schema(json!({ "type": "array", "items": { "type": "string" } }))
                    .agent_tool("bulk_import", "Should never be emitted: body is an array."),
            ]);

        let doc = generate_webmcp(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "an array body must produce no tool: {doc}"
        );
    }

    #[test]
    fn webmcp_skips_an_endpoint_whose_body_is_a_root_recursive_ref() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/x/condition")
                    .summary("Set condition")
                    .auth(AuthLevel::Public)
                    .input_schema(json!({ "$ref": "#" }))
                    .agent_tool(
                        "set_condition",
                        "Should never be emitted: body is a root-recursive $ref.",
                    ),
            ]);

        let doc = generate_webmcp(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "a root-recursive $ref body must produce no tool: {doc}"
        );
    }

    #[test]
    fn webmcp_drops_all_tools_sharing_a_duplicate_name() {
        // Two endpoints both claim `get_thing`. Neither may appear: keeping
        // either one arbitrarily would make the manifest's meaning depend
        // on which endpoint happened to be declared (or iterated) first.
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/a")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_thing", "First get_thing."),
                BlockEndpoint::get("/b/x/b")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_thing", "Second get_thing."),
                BlockEndpoint::get("/b/x/c")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_other_thing", "Uniquely named, must still appear."),
            ]);

        let doc = generate_webmcp(&[block], AuthLevel::Admin);
        assert_eq!(
            tool_names(&doc),
            vec!["get_other_thing".to_string()],
            "both endpoints sharing the duplicated name must be dropped, and the uniquely \
             named endpoint must still appear: {doc}"
        );
    }

    #[test]
    fn webmcp_duplicate_name_drop_is_order_independent() {
        let make_block = |order: [&str; 3]| {
            let by_id = |id: &str| match id {
                "a" => BlockEndpoint::get("/b/x/a")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_thing", "First get_thing."),
                "b" => BlockEndpoint::get("/b/x/b")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_thing", "Second get_thing."),
                "c" => BlockEndpoint::get("/b/x/c")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_other_thing", "Uniquely named."),
                other => panic!("unknown endpoint id: {other}"),
            };
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test")
                .endpoints(order.iter().map(|id| by_id(id)).collect())
        };

        let forward = generate_webmcp(&[make_block(["a", "b", "c"])], AuthLevel::Admin);
        let reversed = generate_webmcp(&[make_block(["c", "b", "a"])], AuthLevel::Admin);

        assert_eq!(tool_names(&forward), vec!["get_other_thing".to_string()]);
        assert_eq!(
            tool_names(&forward),
            tool_names(&reversed),
            "dropping duplicate-named tools must not depend on endpoint declaration order"
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
    fn webmcp_public_manifest_matches_snapshot() {
        // Full-document snapshot at each auth level: exercises the exact
        // output an agent would receive, not just tool names, and is a
        // stronger determinism check than calling the pure function twice
        // in one process (which cannot fail).
        let doc = generate_webmcp(&webmcp_fixture_blocks(), AuthLevel::Public);
        assert_eq!(
            doc,
            json!({
                "schema_version": 1,
                "tools": [
                    {
                        "name": "get_product",
                        "description": "Fetch a product and its offers by id.",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "product_id": { "type": "string" } },
                            "required": ["product_id"]
                        },
                        "invocation": {
                            "method": "get",
                            "path": "/b/products/storefront/{product_id}",
                            "path_params": ["product_id"],
                            "query_params": [],
                            "body_params": []
                        }
                    }
                ]
            })
        );
    }

    #[test]
    fn webmcp_authenticated_manifest_matches_snapshot() {
        let doc = generate_webmcp(&webmcp_fixture_blocks(), AuthLevel::Authenticated);
        assert_eq!(
            doc,
            json!({
                "schema_version": 1,
                "tools": [
                    {
                        "name": "get_product",
                        "description": "Fetch a product and its offers by id.",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "product_id": { "type": "string" } },
                            "required": ["product_id"]
                        },
                        "invocation": {
                            "method": "get",
                            "path": "/b/products/storefront/{product_id}",
                            "path_params": ["product_id"],
                            "query_params": [],
                            "body_params": []
                        }
                    },
                    {
                        "name": "list_my_purchases",
                        "description": "List the signed-in user's purchases.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        },
                        "invocation": {
                            "method": "get",
                            "path": "/b/products/purchases",
                            "path_params": [],
                            "query_params": [],
                            "body_params": []
                        }
                    }
                ]
            })
        );
    }

    #[test]
    fn webmcp_admin_manifest_matches_snapshot() {
        let doc = generate_webmcp(&webmcp_fixture_blocks(), AuthLevel::Admin);
        assert_eq!(
            doc,
            json!({
                "schema_version": 1,
                "tools": [
                    {
                        "name": "get_product",
                        "description": "Fetch a product and its offers by id.",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "product_id": { "type": "string" } },
                            "required": ["product_id"]
                        },
                        "invocation": {
                            "method": "get",
                            "path": "/b/products/storefront/{product_id}",
                            "path_params": ["product_id"],
                            "query_params": [],
                            "body_params": []
                        }
                    },
                    {
                        "name": "list_my_purchases",
                        "description": "List the signed-in user's purchases.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        },
                        "invocation": {
                            "method": "get",
                            "path": "/b/products/purchases",
                            "path_params": [],
                            "query_params": [],
                            "body_params": []
                        }
                    },
                    {
                        "name": "list_users",
                        "description": "List all user accounts.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        },
                        "invocation": {
                            "method": "get",
                            "path": "/b/admin/api/users",
                            "path_params": [],
                            "query_params": [],
                            "body_params": []
                        }
                    }
                ]
            })
        );
    }

    // -----------------------------------------------------------------
    // path templates
    // -----------------------------------------------------------------

    #[test]
    fn path_placeholders_extracts_names_and_rejects_malformed_templates() {
        assert_eq!(path_placeholders("/b/x/list"), Some(Vec::new()));
        assert_eq!(
            path_placeholders("/b/products/storefront/{product_id}"),
            Some(vec!["product_id".to_string()])
        );
        assert_eq!(
            path_placeholders("/b/x/{a}/y/{b}"),
            Some(vec!["a".to_string(), "b".to_string()])
        );

        for malformed in [
            "/b/x/{id",   // unclosed
            "/b/x/id}",   // unopened
            "/b/x/}{id}", // closer before opener
            "/b/x/{}",    // empty name
            "/b/x/{a{b}", // nested opener
        ] {
            assert_eq!(
                path_placeholders(malformed),
                None,
                "`{malformed}` is not a template this function can fill in"
            );
        }
    }

    /// The shape that motivated this check: no production endpoint declares a
    /// `path_params` schema today, so the first annotated templated route
    /// would have shipped a tool whose client GETs a literal `{product_id}`.
    #[test]
    fn webmcp_skips_a_templated_path_with_no_declared_path_params() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/products/storefront/{product_id}")
                    .summary("Storefront product")
                    .auth(AuthLevel::Public)
                    .agent_tool(
                        "get_product",
                        "Should never be emitted: {product_id} unfilled.",
                    ),
            ]);

        let doc = generate_webmcp(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "a placeholder with no declared path param leaves a URL that can never \
             be built, so no tool may be emitted: {doc}"
        );
    }

    #[test]
    fn webmcp_skips_a_templated_path_whose_param_name_disagrees() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/products/storefront/{product_id}")
                    .auth(AuthLevel::Public)
                    .path_params_schema(json!({
                        "type": "object",
                        "properties": { "id": { "type": "string" } },
                        "required": ["id"]
                    }))
                    .agent_tool("get_product", "Should never be emitted: name mismatch."),
            ]);

        let doc = generate_webmcp(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "a declared param whose name does not match the placeholder still \
             leaves `{{product_id}}` unfilled: {doc}"
        );
    }

    #[test]
    fn webmcp_skips_a_declared_path_param_with_no_placeholder() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/products/list")
                    .auth(AuthLevel::Public)
                    .path_params_schema(json!({
                        "type": "object",
                        "properties": { "product_id": { "type": "string" } }
                    }))
                    .agent_tool(
                        "list_products",
                        "Should never be emitted: nowhere to put it.",
                    ),
            ]);

        let doc = generate_webmcp(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "a declared path param with no placeholder to go into would be a \
             required argument the client silently drops: {doc}"
        );
    }

    /// Guard against over-refusing: the check must pass the correct case.
    #[test]
    fn webmcp_emits_a_correctly_matched_templated_path() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/{owner}/items/{item_id}")
                    .auth(AuthLevel::Public)
                    .path_params_schema(json!({
                        "type": "object",
                        "properties": {
                            "owner": { "type": "string" },
                            "item_id": { "type": "string" }
                        },
                        "required": ["owner", "item_id"]
                    }))
                    .agent_tool("get_item", "Fetch one item."),
            ]);

        let doc = generate_webmcp(&[block], AuthLevel::Admin);
        assert_eq!(tool_names(&doc), vec!["get_item".to_string()]);
        assert_eq!(
            doc["tools"][0]["invocation"]["path_params"],
            json!(["item_id", "owner"])
        );
    }

    #[test]
    fn webmcp_emits_a_non_templated_path_with_no_path_params() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/list")
                    .auth(AuthLevel::Public)
                    .agent_tool("list_things", "Nothing to fill in."),
            ]);

        let doc = generate_webmcp(&[block], AuthLevel::Admin);
        assert_eq!(tool_names(&doc), vec!["list_things".to_string()]);
    }

    // -----------------------------------------------------------------
    // effective auth
    // -----------------------------------------------------------------

    fn tiered_block() -> BlockInfo {
        BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
            // Declared Public, but a consumer may mount it under an
            // admin-only prefix and enforce max(prefix, declared).
            BlockEndpoint::get("/b/admin/api/stats")
                .auth(AuthLevel::Public)
                .agent_tool("get_stats", "Instance statistics."),
        ])
    }

    /// A consumer whose router enforces `max(prefix_tier, ep.auth)` cannot be
    /// described by `ep.auth` alone, and `generate_webmcp` structurally
    /// cannot see the prefix table. The resolver is where that knowledge
    /// belongs.
    #[test]
    fn webmcp_with_hides_a_tool_whose_effective_auth_exceeds_the_caller() {
        let blocks = [tiered_block()];
        let with_admin_prefix = |_block: &BlockInfo, ep: &BlockEndpoint| {
            if ep.path.starts_with("/b/admin/") {
                AuthLevel::Admin
            } else {
                ep.auth
            }
        };

        // The declared-auth wrapper trusts `ep.auth` and would publish it.
        assert_eq!(
            tool_names(&generate_webmcp(&blocks, AuthLevel::Public)),
            vec!["get_stats".to_string()],
            "declared-auth-only filtering advertises this to anonymous callers"
        );

        // A resolver that knows about the admin prefix must hide it.
        let doc = generate_webmcp_with(&blocks, AuthLevel::Public, with_admin_prefix);
        assert_eq!(
            doc["tools"],
            json!([]),
            "a tool the router will 403 must not be advertised, and its name is \
             recon surface either way: {doc}"
        );
        let rendered = doc.to_string();
        assert!(
            !rendered.contains("get_stats") && !rendered.contains("/b/admin/api/stats"),
            "nothing about the endpoint may leak: {rendered}"
        );

        // An admin caller is above the effective level and still sees it.
        let doc = generate_webmcp_with(&blocks, AuthLevel::Admin, with_admin_prefix);
        assert_eq!(tool_names(&doc), vec!["get_stats".to_string()]);
    }

    #[test]
    fn webmcp_with_reveals_a_tool_the_resolver_lowers() {
        // The filter runs the other way too: a route the consumer serves
        // without auth must not stay hidden behind a stricter declared level.
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/public-mirror")
                    .auth(AuthLevel::Admin)
                    .agent_tool("read_mirror", "Publicly served by this consumer."),
            ]);
        let blocks = [block];

        assert_eq!(
            generate_webmcp(&blocks, AuthLevel::Public)["tools"],
            json!([]),
            "declared-auth-only filtering hides it"
        );
        assert_eq!(
            tool_names(&generate_webmcp_with(&blocks, AuthLevel::Public, |_, _| {
                AuthLevel::Public
            })),
            vec!["read_mirror".to_string()],
        );
    }

    #[test]
    fn webmcp_passes_the_owning_block_to_the_resolver() {
        // A consumer mounts *blocks* under prefixes, so the resolver has to
        // be able to key off the owning block, not just the endpoint.
        let doc = generate_webmcp_with(&webmcp_fixture_blocks(), AuthLevel::Public, |block, ep| {
            if block.name == "impresspress/products" {
                AuthLevel::Public
            } else {
                ep.auth
            }
        });
        let mut names = tool_names(&doc);
        names.sort();
        assert_eq!(
            names,
            vec!["get_product".to_string(), "list_my_purchases".to_string()],
            "the products block's authenticated endpoint is public under this \
             consumer's routing, and the admin block's is not: {doc}"
        );
    }

    // -----------------------------------------------------------------
    // duplicate names are counted before any filtering
    // -----------------------------------------------------------------

    #[test]
    fn webmcp_duplicate_name_across_auth_levels_is_dropped_for_every_caller() {
        // A name shared by a Public and an Admin endpoint. Counting names
        // after the auth filter would hand a public caller one unambiguous
        // `get_thing` and an admin caller none — privilege would strictly
        // *reduce* the tool set, and the name's meaning would depend on who
        // asked. The name is ambiguous in the declarations, so it is
        // ambiguous for everyone.
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/public")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_thing", "Public get_thing."),
                BlockEndpoint::get("/b/x/admin")
                    .auth(AuthLevel::Admin)
                    .agent_tool("get_thing", "Admin get_thing."),
                BlockEndpoint::get("/b/x/other")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_other_thing", "Uniquely named."),
            ]);
        let blocks = [block];

        for caller in [
            AuthLevel::Public,
            AuthLevel::Authenticated,
            AuthLevel::Admin,
        ] {
            let doc = generate_webmcp(&blocks, caller);
            assert_eq!(
                tool_names(&doc),
                vec!["get_other_thing".to_string()],
                "`get_thing` is duplicated in the declarations, so no caller — \
                 {caller} included — may receive it: {doc}"
            );
        }
    }

    #[test]
    fn webmcp_duplicate_name_still_suppresses_a_side_filtered_out_for_another_reason() {
        // The second endpoint is dropped for a parameter collision. If
        // duplicate counting ran after that skip, the first would silently
        // inherit `get_thing` as though it had always been unique — the
        // collision filter would have laundered the ambiguity away.
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/a")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_thing", "Clean endpoint."),
                BlockEndpoint::post("/b/x/{id}")
                    .auth(AuthLevel::Public)
                    .path_params_schema(json!({
                        "type": "object",
                        "properties": { "id": { "type": "string" } }
                    }))
                    .input_schema(json!({
                        "type": "object",
                        "properties": { "id": { "type": "integer" } }
                    }))
                    .agent_tool("get_thing", "Colliding endpoint."),
            ]);

        let doc = generate_webmcp(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "a duplicated name must stay duplicated even when the other side is \
             dropped for an unrelated reason: {doc}"
        );
    }

    // -----------------------------------------------------------------
    // unresolvable refs below the top level
    // -----------------------------------------------------------------

    #[test]
    fn agent_input_schema_reports_a_nested_unresolvable_ref() {
        // `struct Node { children: Vec<Node> }` — schemars closes the cycle
        // with the root marker `{"$ref": "#"}`, which sits at
        // `properties.children.items`. The top level still has `properties`,
        // so `unrepresentable` stays empty and only the dedicated report
        // catches it.
        let ep = BlockEndpoint::post("/b/x/tree").input_schema(json!({
            "type": "object",
            "properties": {
                "children": { "type": "array", "items": { "$ref": "#" } }
            }
        }));
        let result = agent_input_schema(&ep);
        assert!(
            result.unrepresentable.is_empty(),
            "the top level is a healthy object, which is exactly why the \
             properties-based check cannot see this: {:?}",
            result.unrepresentable
        );
        assert_eq!(result.unresolved_refs, vec!["body".to_string()]);
        assert_eq!(
            result.schema["properties"]["children"]["items"],
            json!({}),
            "and this is what would otherwise have shipped: an unconstrained \
             schema where the server requires a Node"
        );
    }

    #[test]
    fn webmcp_skips_an_endpoint_with_a_nested_root_recursive_ref() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/x/tree")
                    .auth(AuthLevel::Public)
                    .input_schema(json!({
                        "type": "object",
                        "properties": {
                            "children": { "type": "array", "items": { "$ref": "#" } }
                        }
                    }))
                    .agent_tool("set_tree", "Should never be emitted: nested `#` ref."),
            ]);

        let doc = generate_webmcp(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "a `{{}}` hidden one level down is the same lie as one at the top: {doc}"
        );
    }

    #[test]
    fn webmcp_skips_an_endpoint_with_a_ref_to_a_missing_def() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/x/thing")
                    .auth(AuthLevel::Public)
                    .input_schema(json!({
                        "type": "object",
                        "properties": { "status": { "$ref": "#/$defs/Missing" } }
                    }))
                    .agent_tool("set_thing", "Should never be emitted: dangling ref."),
            ]);

        let doc = generate_webmcp(&[block], AuthLevel::Admin);
        assert_eq!(
            doc["tools"],
            json!([]),
            "a ref with no referent must refuse, not degrade to `{{}}`: {doc}"
        );
    }

    #[test]
    fn webmcp_emits_a_tool_whose_ref_name_is_percent_encoded() {
        // The other half of the ref fix: decoding must make legitimately-named
        // types resolve, so this endpoint is published rather than refused.
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::post("/b/x/thing")
                    .auth(AuthLevel::Public)
                    .input_schema(json!({
                        "type": "object",
                        "properties": { "status": { "$ref": "#/$defs/Product%20Status" } },
                        "$defs": {
                            "Product Status": { "type": "string", "enum": ["draft", "active"] }
                        }
                    }))
                    .agent_tool("set_thing", "Set a product status."),
            ]);

        let doc = generate_webmcp(&[block], AuthLevel::Admin);
        assert_eq!(tool_names(&doc), vec!["set_thing".to_string()]);
        assert_eq!(
            doc["tools"][0]["inputSchema"]["properties"]["status"],
            json!({ "type": "string", "enum": ["draft", "active"] })
        );
    }

    // -----------------------------------------------------------------
    // deprecation
    // -----------------------------------------------------------------

    #[test]
    fn webmcp_marks_a_deprecated_tool_without_hiding_it() {
        let block =
            BlockInfo::new("test/block", "1.0.0", "http-handler@v1", "Test").endpoints(vec![
                BlockEndpoint::get("/b/x/old")
                    .auth(AuthLevel::Public)
                    .deprecated()
                    .agent_tool("get_old_thing", "Fetch a thing the old way."),
                BlockEndpoint::get("/b/x/new")
                    .auth(AuthLevel::Public)
                    .agent_tool("get_new_thing", "Fetch a thing."),
            ]);

        let doc = generate_webmcp(&[block], AuthLevel::Admin);
        let tools = doc["tools"].as_array().expect("tools array");

        let old = tools
            .iter()
            .find(|t| t["name"] == "get_old_thing")
            .expect("a deprecated endpoint that still works is still published");
        assert_eq!(old["deprecated"], json!(true));
        assert_eq!(
            old["description"],
            json!("[Deprecated] Fetch a thing the old way."),
            "clients routinely forward only name/description/inputSchema to the \
             model, so the signal has to reach the description too"
        );

        let new = tools
            .iter()
            .find(|t| t["name"] == "get_new_thing")
            .expect("get_new_thing");
        assert!(
            new.get("deprecated").is_none(),
            "a live tool must not carry the key at all: {new}"
        );
        assert_eq!(new["description"], json!("Fetch a thing."));
    }
}
