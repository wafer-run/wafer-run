//! HTTP endpoint declarations — [`BlockEndpoint`] with its builders, plus
//! [`HttpMethod`] and [`AuthLevel`].

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

/// Opt-in metadata marking an endpoint as callable by an agent, with a
/// curated name and description written for *invocation* rather than
/// documentation.
///
/// Absence is meaningful: an endpoint without this is never exposed as a
/// tool, no matter what schemas it carries. Tool names are deliberately
/// independent of the route so renaming a path does not silently rename a
/// tool that agents have learned.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentTool {
    /// Stable tool name exposed to agents (e.g. `get_product`). Must satisfy
    /// [`AgentTool::is_valid_name`]; [`crate::BlockInfo::validate`] enforces
    /// that at registration so boot fails rather than the tool vanishing.
    pub name: String,
    /// Description written to help an agent decide when to call this.
    pub description: String,
}

impl AgentTool {
    /// Longest tool name the MCP tool-name constraint admits.
    pub const MAX_NAME_LEN: usize = 128;

    /// Whether `name` is a legal MCP tool name: non-empty, at most
    /// [`Self::MAX_NAME_LEN`] bytes, and drawn from `[A-Za-z0-9_-]`.
    ///
    /// This is not cosmetic. An MCP client rejects a name outside that set,
    /// and the rejection surfaces inside the consumer's per-tool
    /// registration `try`/`catch` — so the tool simply disappears, with no
    /// error reaching the author, the server, or the agent. An empty name is
    /// worse still: it is a name every unnamed endpoint shares, so the
    /// duplicate-name rule then suppresses *unrelated* tools.
    ///
    /// The set is intentionally the conservative intersection of what MCP
    /// clients accept, rather than anything wider that some client might
    /// tolerate — a name that works in one client and vanishes in another is
    /// the failure this exists to prevent.
    pub fn is_valid_name(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= Self::MAX_NAME_LEN
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
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
    /// Opt-in agent-tool metadata. `None` means never exposed as a tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_tool: Option<AgentTool>,
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
            agent_tool: None,
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
            agent_tool: None,
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

    /// Mark this endpoint as an agent-callable tool with a curated name and
    /// description. Without this call the endpoint is never exposed.
    pub fn agent_tool(mut self, name: &str, description: &str) -> Self {
        self.agent_tool = Some(AgentTool {
            name: name.into(),
            description: description.into(),
        });
        self
    }

    /// Returns true if this endpoint opted in to agent-tool exposure.
    pub fn is_agent_tool(&self) -> bool {
        self.agent_tool.is_some()
    }

    /// Returns true if any schema field is set.
    pub fn has_schema(&self) -> bool {
        self.input_schema.is_some()
            || self.output_schema.is_some()
            || self.path_params.is_some()
            || self.query_params.is_some()
    }

    /// Derive the request-body JSON Schema from `T` via `schemars`.
    ///
    /// Inlined and self-contained: no `$schema`, no `$ref` unless `T` is
    /// recursive. The root `title` is kept — see `self_contained_schema`.
    #[cfg(feature = "json-schema")]
    pub fn input<T: schemars::JsonSchema>(mut self) -> Self {
        self.input_schema = Some(self_contained_schema::<T>());
        self
    }

    /// Derive the response-body JSON Schema from `T` via `schemars`.
    ///
    /// Inlined and self-contained: no `$schema`, no `$ref` unless `T` is
    /// recursive. The root `title` is kept — see `self_contained_schema`.
    #[cfg(feature = "json-schema")]
    pub fn output<T: schemars::JsonSchema>(mut self) -> Self {
        self.output_schema = Some(self_contained_schema::<T>());
        self
    }

    /// Derive the path-params JSON Schema from `T` via `schemars`.
    ///
    /// Inlined and self-contained: no `$schema`, no `$ref` unless `T` is
    /// recursive. The root `title` is kept — see `self_contained_schema`.
    #[cfg(feature = "json-schema")]
    pub fn path_params<T: schemars::JsonSchema>(mut self) -> Self {
        self.path_params = Some(self_contained_schema::<T>());
        self
    }

    /// Derive the query-params JSON Schema from `T` via `schemars`.
    ///
    /// Inlined and self-contained: no `$schema`, no `$ref` unless `T` is
    /// recursive. The root `title` is kept — see `self_contained_schema`.
    #[cfg(feature = "json-schema")]
    pub fn query_params<T: schemars::JsonSchema>(mut self) -> Self {
        self.query_params = Some(self_contained_schema::<T>());
        self
    }
}

/// Derive a JSON Schema for `T` that stands on its own.
///
/// `schemars::schema_for!` produces a *document*: a root schema plus a
/// `$defs` table, wired together with `#/$defs/X` references that resolve
/// against that document's root. Endpoint schemas are never served as
/// documents — they are embedded as a fragment inside an OpenAPI
/// `requestBody`/`responses` object, and `path_params`/`query_params` are
/// taken further apart still, one property at a time, into standalone
/// OpenAPI parameter objects. In both places `#/$defs/X` resolves against
/// the *OpenAPI* root, where no `$defs` exists, so every reference dangles.
///
/// So the generator inlines subschemas instead of referencing them, and one
/// document-level key is suppressed: `$schema`, the meta-schema URI, which
/// is meaningless in an embedded fragment and which no consumer of these
/// schemas reads.
///
/// Field descriptions from `///` doc comments survive: schemars emits them
/// as siblings of the inlined subschema, not as part of the definition they
/// replaced.
///
/// # The root `title` is kept
///
/// schemars fills the root `title` with the Rust type name. That looks like
/// noise, and for an agent reading a WebMCP `inputSchema` it is — but these
/// schemas are also embedded verbatim into `/openapi.json`, where OpenAPI
/// client generators use `title` to *name* the type they generate for the
/// request or response body. Stripping it there degrades every generated
/// client's type names to positional placeholders (`InlineResponse200`), so
/// it stays in the stored schema.
///
/// The WebMCP projection drops it instead, at the point where it is actually
/// noise: `wafer-core`'s `discovery::FLATTENABLE_KEYWORDS` lists `title`
/// among the annotations a source may carry and the merged agent input
/// schema does not reproduce.
///
/// # `$defs` is deliberately *not* removed
///
/// With `inline_subschemas` on, `$defs` is emitted for exactly one reason —
/// a recursive type, which has no finite inlining. schemars closes the cycle
/// with a `$ref`: `"#"` when it closes on the root type (and then there is
/// no `$defs` at all), or `#/$defs/X` plus a matching `$defs` entry when it
/// closes below the root. Deleting the table in that second case would
/// strand the reference, which is the precise failure this function exists
/// to prevent. So whatever referent schemars kept, we keep;
/// `recursive_types_never_reference_a_table_that_was_removed` holds us to
/// it.
///
/// A surviving `$ref` still does not *resolve* inside an OpenAPI document —
/// both `#` and `#/$defs/X` are rooted at the OpenAPI document rather than
/// at the embedded schema. Closing that gap means hoisting definitions into
/// `components/schemas` and rewriting the pointers in `generate_openapi`.
/// Until then `wafer-core`'s `inline_refs` flattens what it can for the
/// WebMCP projection, and recursive contracts are the only shape affected.
#[cfg(feature = "json-schema")]
fn self_contained_schema<T: schemars::JsonSchema>() -> serde_json::Value {
    // Pinned to draft 2020-12 rather than `SchemaSettings::default()`.
    // schemars documents the default as liable to change between minor
    // versions, and this draft is what produces the `#/$defs/X` reference
    // form that `wafer-core::discovery::inline_refs` hardcodes when it
    // flattens these schemas for the WebMCP projection. A default flip to
    // draft-07 would move every reference to `#/definitions/X`, silently
    // resolving none of them, with no compile error anywhere.
    // `$schema` is suppressed by `meta_schema = None`. `title` is kept — see
    // "The root `title` is kept" above.
    schemars::generate::SchemaSettings::draft2020_12()
        .with(|settings| {
            settings.inline_subschemas = true;
            settings.meta_schema = None;
        })
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value()
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

    #[test]
    fn agent_tool_defaults_to_none() {
        let ep = BlockEndpoint::get("/b/products/storefront/{id}").summary("Get product");
        assert!(ep.agent_tool.is_none());
        assert!(!ep.is_agent_tool());
    }

    #[test]
    fn agent_tool_builder_sets_name_and_description() {
        let ep = BlockEndpoint::get("/b/products/storefront/{id}")
            .summary("Get product")
            .agent_tool(
                "get_product",
                "Fetch a product and its purchasable offers by id.",
            );
        let tool = ep.agent_tool.as_ref().expect("agent_tool must be set");
        assert_eq!(tool.name, "get_product");
        assert_eq!(
            tool.description,
            "Fetch a product and its purchasable offers by id."
        );
        assert!(ep.is_agent_tool());
    }

    #[test]
    fn agent_tool_is_omitted_from_json_when_absent() {
        let ep = BlockEndpoint::get("/health").summary("Health check");
        let json = serde_json::to_value(&ep).expect("serialize");
        assert!(
            json.get("agent_tool").is_none(),
            "absent agent_tool must not appear in serialized output: {json}"
        );
    }

    #[cfg(feature = "json-schema")]
    #[test]
    fn derived_input_schema_is_self_contained() {
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        enum Status {
            Draft,
            Active,
        }

        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct CreateProduct {
            /// Human-readable product name.
            name: String,
            /// Current lifecycle status.
            status: Status,
        }

        let ep = BlockEndpoint::post("/b/products").input::<CreateProduct>();
        let schema = ep.input_schema.expect("input schema set");
        let rendered = schema.to_string();

        assert!(
            !rendered.contains("$ref"),
            "derived schemas are embedded into OpenAPI documents where #/$defs \
             does not resolve — no $ref may survive: {rendered}"
        );
        assert!(
            !rendered.contains("$defs"),
            "the $defs table must not travel with the schema: {rendered}"
        );
        assert!(
            schema.get("$schema").is_none(),
            "root $schema is meaningless inside an OpenAPI requestBody: {rendered}"
        );
        assert_eq!(
            schema["title"],
            serde_json::json!("CreateProduct"),
            "the root title names the generated type in /openapi.json and \
             must survive: {rendered}"
        );
    }

    /// The stored schema is embedded verbatim into `/openapi.json`, where
    /// OpenAPI client generators read the root `title` to name the type they
    /// generate for the body. Both the schemars default (the Rust type name)
    /// and an explicit `#[schemars(title = "...")]` are therefore kept;
    /// dropping either degrades generated client type names to positional
    /// placeholders. The WebMCP projection drops the title on its own side,
    /// where the Rust type name is genuinely noise for an agent.
    #[cfg(feature = "json-schema")]
    #[test]
    fn derived_schema_keeps_its_root_title() {
        #[derive(schemars::JsonSchema)]
        #[schemars(title = "Create a product")]
        #[allow(dead_code)]
        struct TitledProduct {
            name: String,
        }

        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct UntitledProduct {
            name: String,
        }

        let titled = BlockEndpoint::post("/b/products")
            .input::<TitledProduct>()
            .input_schema
            .expect("input schema set");
        assert_eq!(
            titled["title"],
            serde_json::json!("Create a product"),
            "an explicit #[schemars(title = ...)] must survive: {titled}"
        );

        let untitled = BlockEndpoint::post("/b/products")
            .input::<UntitledProduct>()
            .input_schema
            .expect("input schema set");
        assert_eq!(
            untitled["title"],
            serde_json::json!("UntitledProduct"),
            "the schemars-default title names the generated OpenAPI type and \
             must survive: {untitled}"
        );
    }

    #[cfg(feature = "json-schema")]
    #[test]
    fn derived_schema_keeps_field_descriptions() {
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct WithDocs {
            /// Human-readable product name.
            name: String,
        }

        let ep = BlockEndpoint::post("/b/x").input::<WithDocs>();
        let schema = ep.input_schema.expect("input schema set");
        assert_eq!(
            schema["properties"]["name"]["description"],
            serde_json::json!("Human-readable product name."),
            "doc comments must reach the schema — the derive migration relies \
             on this to preserve editorial text: {schema}"
        );
    }

    #[cfg(feature = "json-schema")]
    #[test]
    fn derived_schema_keeps_descriptions_on_inlined_named_types() {
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        enum Status {
            Draft,
            Active,
        }

        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct WithNamedField {
            /// Current lifecycle status.
            status: Status,
        }

        let ep = BlockEndpoint::post("/b/x").input::<WithNamedField>();
        let schema = ep.input_schema.expect("input schema set");
        assert_eq!(
            schema["properties"]["status"]["description"],
            serde_json::json!("Current lifecycle status."),
            "inlining a named type must not swallow the field's own doc \
             comment: {schema}"
        );
        assert_eq!(
            schema["properties"]["status"]["enum"],
            serde_json::json!(["Draft", "Active"]),
            "the named type's own schema must be inlined in place: {schema}"
        );
    }

    #[cfg(feature = "json-schema")]
    #[test]
    fn derived_query_params_schema_inlines_enums() {
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        enum SortOrder {
            Asc,
            Desc,
        }

        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct ListQuery {
            sort: Option<SortOrder>,
        }

        let ep = BlockEndpoint::get("/b/x").query_params::<ListQuery>();
        let schema = ep.query_params.expect("query params schema set");
        assert!(
            !schema.to_string().contains("$ref"),
            "extract_params lifts each property out standalone and drops $defs, \
             so an enum-typed query param must already be inlined: {schema}"
        );
    }

    #[cfg(feature = "json-schema")]
    #[test]
    fn derived_output_and_path_params_are_self_contained() {
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        enum Kind {
            One,
            Two,
        }

        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct Payload {
            kind: Kind,
        }

        let ep = BlockEndpoint::get("/b/x/{id}")
            .output::<Payload>()
            .path_params::<Payload>();
        for (label, schema) in [
            ("output", ep.output_schema.expect("output schema set")),
            ("path_params", ep.path_params.expect("path params set")),
        ] {
            let rendered = schema.to_string();
            assert!(
                !rendered.contains("$ref") && !rendered.contains("$defs"),
                "{label} schema must stand alone: {rendered}"
            );
            assert!(
                schema.get("$schema").is_none(),
                "{label} schema must not carry the meta-schema URI: {rendered}"
            );
            assert_eq!(
                schema["title"],
                serde_json::json!("Payload"),
                "{label} schema keeps its root title for /openapi.json: {rendered}"
            );
        }
    }

    /// Walk a schema and collect every `$ref` string it contains.
    #[cfg(feature = "json-schema")]
    fn collect_refs(node: &serde_json::Value, out: &mut Vec<String>) {
        match node {
            serde_json::Value::Object(map) => {
                for (key, value) in map {
                    if key == "$ref" {
                        if let Some(reference) = value.as_str() {
                            out.push(reference.to_string());
                        }
                    }
                    collect_refs(value, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    collect_refs(item, out);
                }
            }
            _ => {}
        }
    }

    /// Recursion is the one shape `inline_subschemas` cannot fully resolve, and
    /// this test pins down exactly what escapes so a schemars upgrade cannot
    /// change it silently.
    ///
    /// **This is not the guarantee the other `derived_*` tests give.** Those
    /// assert no `$ref` at all; recursive contracts still emit one. What is
    /// guaranteed here is narrower but the one that matters for correctness:
    /// *a surviving `$ref` always has its referent.* `#` is the schema's own
    /// root and needs no table; `#/$defs/X` comes with `$defs.X` still
    /// attached. Nothing is ever left pointing at a table this builder
    /// deleted.
    ///
    /// The remaining gap is a *consumer* problem: inside an OpenAPI document
    /// both forms resolve against the OpenAPI root rather than the embedded
    /// schema. Closing it means hoisting `$defs` into `components/schemas` and
    /// rewriting the pointers in `generate_openapi`, which is a deliberate
    /// change to a live surface, not something to smuggle in here.
    #[cfg(feature = "json-schema")]
    #[test]
    fn recursive_types_never_reference_a_table_that_was_removed() {
        /// A condition tree — the shape that makes this test necessary.
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct Condition {
            all_of: Vec<Condition>,
        }

        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct Rule {
            /// The condition tree.
            when: Condition,
        }

        // Directly recursive at the root: schemars closes the cycle with `#`,
        // a pointer to the schema's own root, and emits no `$defs` at all.
        let root = BlockEndpoint::post("/b/x")
            .input::<Condition>()
            .input_schema
            .expect("input schema set");
        let mut refs = Vec::new();
        collect_refs(&root, &mut refs);
        assert_eq!(refs, vec!["#".to_string()], "root-recursive shape: {root}");
        assert!(
            root.get("$defs").is_none(),
            "a `#` cycle-break needs no definitions table: {root}"
        );

        // Recursive below the root: schemars cannot use `#` (that is `Rule`,
        // not `Condition`), so it names the type and emits a `$defs` entry.
        // Deleting that table here — as the naive `obj.remove("$defs")` would
        // — is exactly the dangling reference this whole change exists to
        // prevent, so the table stays.
        let nested = BlockEndpoint::post("/b/x")
            .input::<Rule>()
            .input_schema
            .expect("input schema set");
        let mut refs = Vec::new();
        collect_refs(&nested, &mut refs);
        assert!(
            !refs.is_empty(),
            "nested recursion is expected to leave a ref: {nested}"
        );
        for reference in &refs {
            let name = reference
                .strip_prefix("#/$defs/")
                .unwrap_or_else(|| panic!("unexpected ref form {reference}: {nested}"));
            assert!(
                nested
                    .get("$defs")
                    .and_then(|defs| defs.get(name))
                    .is_some(),
                "`{reference}` must still have its referent: {nested}"
            );
        }

        // The field's doc comment survives the partial inlining too.
        assert_eq!(
            nested["properties"]["when"]["description"],
            serde_json::json!("The condition tree."),
            "descriptions must survive on recursive fields as well: {nested}"
        );
    }

    /// `$defs` is a recursion-only escape hatch, never a routine emission.
    /// If this ever fails, some non-recursive contract started shipping a
    /// reference table and the `derived_*` guarantees have quietly narrowed.
    #[cfg(feature = "json-schema")]
    #[test]
    fn non_recursive_types_never_emit_a_definitions_table() {
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        enum Currency {
            Usd,
            Eur,
        }

        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct Money {
            amount: i64,
            currency: Currency,
        }

        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct Order {
            /// What the buyer pays.
            total: Money,
            /// Line item prices.
            lines: Vec<Money>,
            refund: Option<Money>,
        }

        let schema = BlockEndpoint::post("/b/orders")
            .input::<Order>()
            .input_schema
            .expect("input schema set");
        let rendered = schema.to_string();
        assert!(
            !rendered.contains("$defs") && !rendered.contains("$ref"),
            "a type repeated three times must be inlined three times, not \
             referenced: {rendered}"
        );
        assert_eq!(
            schema["properties"]["total"]["properties"]["currency"]["enum"],
            serde_json::json!(["Usd", "Eur"]),
            "nested named types must be inlined transitively: {rendered}"
        );
        assert_eq!(
            schema["properties"]["lines"]["description"],
            serde_json::json!("Line item prices."),
            "descriptions survive alongside inlined array items: {rendered}"
        );
    }

    #[test]
    fn agent_tool_round_trips_through_serde() {
        let ep = BlockEndpoint::post("/b/products/checkout")
            .summary("Stripe checkout")
            .agent_tool("start_checkout", "Create a Stripe Checkout Session.");
        let json = serde_json::to_value(&ep).expect("serialize");
        let back: BlockEndpoint = serde_json::from_value(json).expect("deserialize");
        let tool = back
            .agent_tool
            .as_ref()
            .expect("agent_tool survives round-trip");
        assert_eq!(tool.name, "start_checkout");
        assert_eq!(tool.description, "Create a Stripe Checkout Session.");
    }
}
