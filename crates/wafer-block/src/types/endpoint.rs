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
///
/// # Ordering is the strictness ladder
///
/// `Public < Authenticated < Admin`. The derived `Ord` follows **declaration
/// order**, and the variants are declared weakest-requirement first
/// deliberately so that the derive expresses that ladder: a caller holding
/// level `caller` may see exactly the endpoints whose requirement satisfies
/// `required <= caller`.
///
/// This makes variant order load-bearing for access decisions. Adding a
/// variant is only safe in the position its strictness dictates — appending
/// a level *weaker* than `Admin` at the end, or inserting one without
/// renumbering intent, silently changes who can see what. The exhaustive
/// ladder in `auth_level_order_tests` exists to stop that: it fails to
/// compile when a variant is added and fails at run time when one is
/// misplaced.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
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
    /// The endpoint needs something only a server holds (a secret key, say),
    /// so it cannot work when the runtime runs in a browser.
    ///
    /// wafer only records the flag: nothing in wafer reads it, and
    /// `wafer-core`'s discovery documents list a flagged endpoint like any
    /// other. The *host* decides when it applies — impresspress, for one,
    /// leaves flagged endpoints out of discovery in its browser runtime. It
    /// is never an access control: the handler stays the gate.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub server_only: bool,
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
            server_only: false,
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
            server_only: false,
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

    /// Mark this endpoint as needing something only a server holds (a
    /// secret key, say) — see the `server_only` field. wafer records the
    /// flag; the host decides when to act on it (a browser runtime can leave
    /// the endpoint out of its discovery documents), and the handler stays
    /// the gate.
    pub fn server_only(mut self) -> Self {
        self.server_only = true;
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
    ///
    /// Generated under the **deserialize** contract: a request body is what a
    /// client sends and the server deserializes.
    #[cfg(feature = "json-schema")]
    pub fn input<T: schemars::JsonSchema>(mut self) -> Self {
        self.input_schema = Some(self_contained_schema::<T>(
            schemars::generate::Contract::Deserialize,
        ));
        self
    }

    /// Derive the response-body JSON Schema from `T` via `schemars`.
    ///
    /// Inlined and self-contained: no `$schema`, no `$ref` unless `T` is
    /// recursive. The root `title` is kept — see `self_contained_schema`.
    ///
    /// Generated under the **serialize** contract — the one builder that is.
    /// A response body is what the server *serializes*, so the schema must
    /// describe what the server guarantees to emit, not what it would accept.
    /// See `self_contained_schema`'s "Which contract" section.
    #[cfg(feature = "json-schema")]
    pub fn output<T: schemars::JsonSchema>(mut self) -> Self {
        self.output_schema = Some(self_contained_schema::<T>(
            schemars::generate::Contract::Serialize,
        ));
        self
    }

    /// Derive the path-params JSON Schema from `T` via `schemars`.
    ///
    /// Inlined and self-contained: no `$schema`, no `$ref` unless `T` is
    /// recursive. The root `title` is kept — see `self_contained_schema`.
    ///
    /// Generated under the **deserialize** contract: path params are what a
    /// client sends and the server deserializes.
    #[cfg(feature = "json-schema")]
    pub fn path_params<T: schemars::JsonSchema>(mut self) -> Self {
        self.path_params = Some(self_contained_schema::<T>(
            schemars::generate::Contract::Deserialize,
        ));
        self
    }

    /// Derive the query-params JSON Schema from `T` via `schemars`.
    ///
    /// Inlined and self-contained: no `$schema`, no `$ref` unless `T` is
    /// recursive. The root `title` is kept — see `self_contained_schema`.
    ///
    /// Generated under the **deserialize** contract: query params are what a
    /// client sends and the server deserializes.
    #[cfg(feature = "json-schema")]
    pub fn query_params<T: schemars::JsonSchema>(mut self) -> Self {
        self.query_params = Some(self_contained_schema::<T>(
            schemars::generate::Contract::Deserialize,
        ));
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
/// # A recursive type is referenced, not unrolled
///
/// schemars' inlining is decided per use site and cannot see ahead: the
/// first time it meets a type it inlines the whole body, and only when that
/// body reaches the type *again* does it fall back to a reference and put
/// the body in `$defs`. So a recursive type below the root ends up with its
/// full body pasted in at every use site *and* once more in `$defs` — a
/// condition tree used in three fields carries its body four times. There
/// is no setting that changes this: `inline_subschemas` is all-or-nothing,
/// and turning it off references *every* named type, which also changes the
/// shape of every non-recursive one (`Option<Struct>` becomes an
/// `anyOf: [{"$ref": ...}, {"type": "null"}]` instead of a nullable inlined
/// object), the opposite of what the `derived_*` tests guarantee.
///
/// So [`reference_recursive_definitions`] finishes the job schemars stopped
/// halfway through: every subschema that is a copy of a `$defs` body becomes
/// a `$ref` to it, keeping the use site's own annotations (a field's doc
/// comment, its `default`) beside the reference — legal next to `$ref` in
/// draft 2020-12, and what schemars itself emits for a non-inlined field.
/// Every type schemars kept in `$defs` then appears exactly once; types it
/// did not keep are untouched and stay fully inlined.
/// `recursive_types_are_referenced_not_unrolled` holds us to it, and
/// `a_recursive_type_inside_another_is_referenced_too` holds the nested case.
///
/// Out of scope: a type whose recursion closes on the *root* — schemars'
/// `{"$ref": "#"}`, with no `$defs` entry — has no body here to reference.
/// Used as a field of some *other* root, that same type closes on
/// `#/$defs/X` and is covered; as the root itself its body is the document,
/// and any copy of it inside (none in schemars' own output, which emits `#`
/// at every re-entry) would stay as it is.
///
/// A surviving `$ref` still does not *resolve* inside an OpenAPI document —
/// both `#` and `#/$defs/X` are rooted at the OpenAPI document rather than
/// at the embedded schema, which is why `wafer_core::discovery::generate_openapi`
/// hoists definitions into `components/schemas` (and, for a root that
/// references itself, the root too) and rewrites the pointers before
/// embedding a schema in the document; `wafer-core`'s `inline_refs`
/// builds the WebMCP projection by inlining every acyclic definition and
/// carrying each cyclic one once in the projection's own `$defs`, rebasing
/// `#` onto a named entry there.
///
/// # Which contract
///
/// schemars generates a schema under one of two *contracts*: `Deserialize`
/// (the default) describes what a value may look like on the way *in*;
/// `Serialize` describes what it looks like on the way *out*. They are not
/// the same document, and picking the wrong one distorts the result in a
/// consistent direction.
///
/// `input`, `path_params` and `query_params` all describe what a client
/// sends and the server deserializes, so they take `Deserialize`. `output`
/// describes what the server serializes and the client reads, so it takes
/// `Serialize`. Under `Deserialize` a response schema *understates what the
/// server guarantees* — it tells an agent or a generated client that a field
/// may be absent when the server always emits it. Concretely, for `output`
/// the serialize contract fixes:
///
/// - `#[serde(default)]` — deserialize drops the field from `required`
///   (a client may omit it); serialize keeps it there, because the server
///   always emits a value, defaulted or not.
/// - a plain `Option<T>` with no `skip_serializing_if` — deserialize makes it
///   optional; serialize makes it `required` *and* nullable, which is exactly
///   right: serde emits the key with `null` for `None`, so the key is always
///   present.
/// - `#[serde(skip_deserializing)]` — a server-computed field absent from the
///   deserialize schema entirely; serialize includes it, marked `readOnly`.
/// - `#[serde(skip_serializing_if = "...")]` on a non-`Option` (say
///   `Vec::is_empty`) — deserialize marks it `required`; serialize makes it
///   optional, because the server may omit the key.
/// - `#[serde(into = "...")]` — serialize describes the `into` type, which is
///   what actually goes on the wire.
///
/// ## What it does *not* fix
///
/// A `#[serde(skip_serializing_if = "Option::is_none")]` `Option<T>` still
/// renders as `"type": ["T", "null"]` under **both** contracts (schemars
/// 1.2.1). The server never emits `null` for such a field — it emits a `T` or
/// omits the key — so the `null` branch is spurious, but the serialize
/// contract does not remove it; `Option<T>`'s `JsonSchema` impl calls
/// `allow_null` unconditionally. The test
/// `contract_narrows_a_skip_serializing_if_option_only_to_optional` pins the
/// current behaviour, so a schemars upgrade that *does* fix it is noticed
/// rather than absorbed silently.
///
/// The narrowing that *is* available today is `#[schemars(required)]` on the
/// field, which is **not** inert: it drops the `null` branch under either
/// contract. Under the serialize contract it does not also force the property
/// into `required` — `skip_serializing_if` still means the server may omit
/// the key — so the pair yields exactly the right response schema for this
/// shape: optional, and non-null when present.
#[cfg(feature = "json-schema")]
fn self_contained_schema<T: schemars::JsonSchema>(
    contract: schemars::generate::Contract,
) -> serde_json::Value {
    // Pinned to draft 2020-12 rather than `SchemaSettings::default()`.
    // schemars documents the default as liable to change between minor
    // versions, and this draft is what produces the `#/$defs/X` reference
    // form that `wafer-core::discovery::inline_refs` hardcodes when it
    // flattens these schemas for the WebMCP projection. A default flip to
    // draft-07 would move every reference to `#/definitions/X`, silently
    // resolving none of them, with no compile error anywhere.
    // `$schema` is suppressed by `meta_schema = None`. `title` is kept — see
    // "The root `title` is kept" above. `contract` is the caller's — see
    // "Which contract" above; it changes only which of the two documents
    // schemars generates, never the inlining, the meta-schema suppression, or
    // the `$defs`/`$ref` form (both contracts emit identical references for a
    // recursive type, so `recursive_types_never_reference_a_table_that_was_removed`
    // holds under either).
    let mut schema = schemars::generate::SchemaSettings::draft2020_12()
        .with(|settings| {
            settings.inline_subschemas = true;
            settings.meta_schema = None;
            settings.contract = contract;
        })
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value();
    reference_recursive_definitions(&mut schema);
    schema
}

/// Keywords that annotate a schema without constraining what it accepts —
/// the ones schemars sets from a *use site* (a field's doc comment, its
/// `#[serde(default)]`, `#[deprecated]`, ...) on top of the type's own body.
/// Two schemas that differ only in these accept exactly the same instances.
#[cfg(feature = "json-schema")]
const ANNOTATION_KEYWORDS: &[&str] = &[
    "title",
    "description",
    "default",
    "examples",
    "deprecated",
    "readOnly",
    "writeOnly",
    "$comment",
];

/// Replace every copy of a `$defs` body with a `$ref` to it — see "A
/// recursive type is referenced, not unrolled" on [`self_contained_schema`].
///
/// A subschema is a copy of definition `X` when, ignoring
/// [`ANNOTATION_KEYWORDS`] at its top level, it is equal to `X`'s body. Such
/// a copy accepts exactly what `X` accepts, so the substitution is sound
/// whatever produced the copy; the use site's annotations that differ from
/// the body's are kept beside the `$ref`.
///
/// The walk follows schema positions only — the keywords
/// [`super::keyword_value`] classifies as holding subschemas — so literal
/// instance data under `default`, `const`, `enum` or `examples` is never
/// rewritten, and neither is the value of a keyword it does not know. It is
/// bottom-up, so a copy whose own nested copies have already collapsed is
/// recognised. The bodies themselves are collapsed first, to a fixpoint: a
/// definition can contain a copy of another (a recursive type nested inside
/// an unrelated recursive type), and a copy elsewhere only matches the body
/// once both are in the same, collapsed form. Neither the document root nor
/// a definition's own top level is ever replaced — they are what references
/// point *at*.
#[cfg(feature = "json-schema")]
fn reference_recursive_definitions(schema: &mut serde_json::Value) {
    let Some(mut defs) = schema
        .get("$defs")
        .and_then(serde_json::Value::as_object)
        .cloned()
    else {
        return;
    };

    loop {
        let mut changed = false;
        let names: Vec<String> = defs.keys().cloned().collect();
        for name in names {
            let mut body = defs[&name].clone();
            if collapse_subschemas(&mut body, &defs) {
                defs.insert(name, body);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // `$defs` is not a subschema keyword, so the walk from the root does not
    // enter the table; it is written back entry by entry so its position in
    // the document (and every key order) is unchanged.
    collapse_subschemas(schema, &defs);
    if let Some(table) = schema
        .get_mut("$defs")
        .and_then(serde_json::Value::as_object_mut)
    {
        for (name, body) in defs {
            table.insert(name, body);
        }
    }
}

/// Collapse copies of `defs` bodies among the subschemas *below* `schema`,
/// leaving `schema` itself in place. Returns whether anything changed.
#[cfg(feature = "json-schema")]
fn collapse_subschemas(
    schema: &mut serde_json::Value,
    defs: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    let serde_json::Value::Object(map) = schema else {
        return false;
    };
    let mut changed = false;
    for (key, value) in map.iter_mut() {
        // Instance data (`Literal`) and anything not known to hold subschemas
        // (`Other`) are left alone: a copy left uncollapsed only stays
        // inlined, whereas rewriting instance data would change what the
        // schema says.
        match super::keyword_value(key) {
            super::KeywordValue::Subschema => changed |= collapse(value, defs),
            super::KeywordValue::SubschemaList => {
                if let serde_json::Value::Array(items) = value {
                    for item in items {
                        changed |= collapse(item, defs);
                    }
                }
            }
            super::KeywordValue::SubschemaMap => {
                if let serde_json::Value::Object(members) = value {
                    for member in members.values_mut() {
                        changed |= collapse(member, defs);
                    }
                }
            }
            super::KeywordValue::Literal | super::KeywordValue::Other => {}
        }
    }
    changed
}

/// Collapse below `schema`, then `schema` itself if it is now a copy of a
/// definition. Returns whether anything changed.
#[cfg(feature = "json-schema")]
fn collapse(
    schema: &mut serde_json::Value,
    defs: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    let mut changed = collapse_subschemas(schema, defs);
    if let Some(reference) = reference_for_copy(schema, defs) {
        *schema = reference;
        changed = true;
    }
    changed
}

/// The `$ref` that replaces `schema`, if `schema` is a copy of one of
/// `defs`' bodies.
///
/// Besides an exact copy, this recognises the one other shape schemars gives
/// a use site of a recursive type: `Option<X>`. schemars makes a schema
/// nullable (`allow_null`, schemars 1.2) by wrapping it in
/// `anyOf: [<schema>, {"type": "null"}]` when it has an `if`/`allOf`/
/// `anyOf`/`oneOf`/`$ref` of its own — whose first member is then an exact
/// copy and collapses on its own — and otherwise by adding `"null"` to its
/// `type`. That second form is matched via [`nullable`] and becomes the
/// `anyOf` form schemars emits for a referenced `Option<X>`.
#[cfg(feature = "json-schema")]
fn reference_for_copy(
    schema: &serde_json::Value,
    defs: &serde_json::Map<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    let copy = schema.as_object()?;
    if copy.contains_key("$ref") {
        return None;
    }
    for (name, body) in defs {
        let Some(body) = body.as_object() else {
            continue;
        };
        let target =
            serde_json::json!({ "$ref": format!("#/$defs/{}", super::encode_ref_name(name)) });
        let reference = if same_constraints(copy, body) {
            target
        } else if nullable(body).is_some_and(|nullable| same_constraints(copy, &nullable)) {
            serde_json::json!({ "anyOf": [target, { "type": "null" }] })
        } else {
            continue;
        };
        let serde_json::Value::Object(mut reference) = reference else {
            unreachable!("built as an object above");
        };
        for key in ANNOTATION_KEYWORDS {
            if let Some(value) = copy.get(*key) {
                if body.get(*key) != Some(value) {
                    reference.insert((*key).to_string(), value.clone());
                }
            }
        }
        return Some(serde_json::Value::Object(reference));
    }
    None
}

/// Whether two schema objects are equal once their top-level
/// [`ANNOTATION_KEYWORDS`] are ignored.
#[cfg(feature = "json-schema")]
fn same_constraints(
    a: &serde_json::Map<String, serde_json::Value>,
    b: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    let constraints = |map: &serde_json::Map<String, serde_json::Value>| {
        map.iter()
            .filter(|(key, _)| !ANNOTATION_KEYWORDS.contains(&key.as_str()))
            .count()
    };
    constraints(a) == constraints(b)
        && a.iter()
            .filter(|(key, _)| !ANNOTATION_KEYWORDS.contains(&key.as_str()))
            .all(|(key, value)| b.get(key) == Some(value))
}

/// Keywords that constrain only instances of some *other* type than `null`
/// (an object, array, string or number) and pass every other instance
/// through. A schema built from `type` plus these alone accepts `null`
/// exactly when its `type` lists `"null"` — which is what makes the
/// type-widened copy in [`nullable`] equivalent to
/// `anyOf: [{"$ref": body}, {"type": "null"}]`.
#[cfg(feature = "json-schema")]
const NULL_TRANSPARENT_KEYWORDS: &[&str] = &[
    "properties",
    "patternProperties",
    "additionalProperties",
    "unevaluatedProperties",
    "propertyNames",
    "required",
    "dependentRequired",
    "dependentSchemas",
    "minProperties",
    "maxProperties",
    "items",
    "prefixItems",
    "unevaluatedItems",
    "contains",
    "minContains",
    "maxContains",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "contentEncoding",
    "contentMediaType",
    "contentSchema",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
];

/// `body` as schemars' `allow_null` renders it in place when it has no
/// subschema keyword of its own: `"null"` added to its `type`.
///
/// `None` — so a nullable copy stays inlined — unless that widened copy is
/// *exactly* equivalent to `anyOf: [{"$ref": body}, {"type": "null"}]`,
/// i.e. unless every constraint besides `type` is an annotation or one of
/// the [`NULL_TRANSPARENT_KEYWORDS`]. A keyword that can reject `null` on
/// its own — `not`, `enum`, `const`, a composition keyword, an extension
/// nobody here can read — would make the copy reject `null` where the
/// `anyOf` form admits it. (`allow_null` wraps a body with
/// `if`/`allOf`/`anyOf`/`oneOf`/`$ref` in `anyOf` itself; that copy's first
/// member is exact and collapses on its own.) Also `None` with no `type` to
/// widen, or one that already admits `null`: such a copy is recognised by
/// plain equality or not at all.
#[cfg(feature = "json-schema")]
fn nullable(
    body: &serde_json::Map<String, serde_json::Value>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let null_transparent = body.keys().all(|key| {
        let key = key.as_str();
        key == "type"
            || ANNOTATION_KEYWORDS.contains(&key)
            || NULL_TRANSPARENT_KEYWORDS.contains(&key)
    });
    if !null_transparent {
        return None;
    }
    let mut widened = body.clone();
    match widened.get_mut("type")? {
        serde_json::Value::String(single) if single != "null" => {
            let single = std::mem::take(single);
            widened.insert("type".into(), serde_json::json!([single, "null"]));
        }
        serde_json::Value::Array(types) if !types.contains(&serde_json::json!("null")) => {
            types.push(serde_json::json!("null"));
        }
        _ => return None,
    }
    Some(widened)
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

    /// Does `schema` list `field` in its `required` array?
    #[cfg(feature = "json-schema")]
    fn is_required(schema: &serde_json::Value, field: &str) -> bool {
        schema["required"]
            .as_array()
            .is_some_and(|req| req.iter().any(|v| v.as_str() == Some(field)))
    }

    /// A struct exercising the shapes where the two contracts disagree.
    ///
    /// Used by the `output_contract_*` tests below, which each assert the
    /// *difference* between `.input::<T>()` and `.output::<T>()` — never one
    /// side alone, so that flipping both builders to the same contract fails
    /// the suite rather than passing it.
    #[cfg(feature = "json-schema")]
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct ContractProbe {
        /// Always sent, always emitted.
        id: String,
        /// Client may omit it; the server always emits a value.
        #[serde(default)]
        count: u32,
        /// Client may omit it; the server emits `null` for `None`.
        label: Option<String>,
        /// Client may omit it; the server omits the key for `None`.
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    }

    /// A `#[serde(default)]` field is optional to a *sender* and guaranteed by
    /// the *server*. The deserialize contract sees only the first half, which
    /// is why a response schema built from it understates the response.
    #[cfg(feature = "json-schema")]
    #[test]
    fn output_contract_makes_a_serde_default_field_required() {
        let input = BlockEndpoint::post("/b/x")
            .input::<ContractProbe>()
            .input_schema
            .expect("input schema set");
        let output = BlockEndpoint::post("/b/x")
            .output::<ContractProbe>()
            .output_schema
            .expect("output schema set");

        assert!(
            is_required(&output, "count"),
            "the server always emits a #[serde(default)] field, so the response \
             schema must guarantee it: {output}"
        );
        assert!(
            !is_required(&input, "count"),
            "a client may omit a #[serde(default)] field, so the request schema \
             must not demand it: {input}"
        );
    }

    /// A plain `Option<T>` with no `skip_serializing_if` is the case the
    /// deserialize contract gets *most* wrong for a response: serde emits the
    /// key with `null` for `None`, so the key is always present. The serialize
    /// contract says `required` and nullable — both halves true.
    #[cfg(feature = "json-schema")]
    #[test]
    fn output_contract_requires_a_plain_option_the_input_leaves_optional() {
        let input = BlockEndpoint::post("/b/x")
            .input::<ContractProbe>()
            .input_schema
            .expect("input schema set");
        let output = BlockEndpoint::post("/b/x")
            .output::<ContractProbe>()
            .output_schema
            .expect("output schema set");

        assert!(
            is_required(&output, "label"),
            "serde emits `\"label\": null` for None, so the key is always \
             present in a response: {output}"
        );
        assert!(
            !is_required(&input, "label"),
            "a client may omit a plain Option field: {input}"
        );
        // Nullable on both sides — `null` really is a value this field takes.
        for (label, schema) in [("input", &input), ("output", &output)] {
            assert_eq!(
                schema["properties"]["label"]["type"],
                serde_json::json!(["string", "null"]),
                "{label}: a plain Option stays nullable — None is emitted as \
                 null: {schema}"
            );
        }
    }

    /// A field with no `default` and no `skip_serializing_if` is required
    /// under both contracts. This is the control: it must *not* move, or the
    /// tests above would be measuring something other than the contract.
    #[cfg(feature = "json-schema")]
    #[test]
    fn a_plain_field_is_required_under_both_contracts() {
        let input = BlockEndpoint::post("/b/x")
            .input::<ContractProbe>()
            .input_schema
            .expect("input schema set");
        let output = BlockEndpoint::post("/b/x")
            .output::<ContractProbe>()
            .output_schema
            .expect("output schema set");

        assert!(is_required(&input, "id"), "request: {input}");
        assert!(is_required(&output, "id"), "response: {output}");
    }

    /// `#[serde(skip_serializing_if = "Option::is_none")]` on an `Option<T>`
    /// is the one shape the serialize contract does **not** repair, and this
    /// test exists to keep that honest rather than let anyone assume it was
    /// fixed along with the rest.
    ///
    /// The server emits either a `T` or no key at all — it never emits `null`
    /// — so `"type": ["T", "null"]` is spurious in a response. schemars 1.2.1
    /// emits it anyway under both contracts: `Option<T>`'s `JsonSchema` impl
    /// calls `allow_null` without consulting the contract. What the serialize
    /// contract *does* get right is the `required` half — the key may be
    /// absent, and it says so.
    ///
    /// If a schemars upgrade starts dropping the `null` branch under the
    /// serialize contract, this test fails. That failure is good news: delete
    /// the nullability assertion and the "What it does not fix" paragraph on
    /// `self_contained_schema`.
    #[cfg(feature = "json-schema")]
    #[test]
    fn contract_narrows_a_skip_serializing_if_option_only_to_optional() {
        let input = BlockEndpoint::post("/b/x")
            .input::<ContractProbe>()
            .input_schema
            .expect("input schema set");
        let output = BlockEndpoint::post("/b/x")
            .output::<ContractProbe>()
            .output_schema
            .expect("output schema set");

        // Optional under both — correctly so, for different reasons: the
        // client may omit the key, and so may the server.
        assert!(!is_required(&input, "note"), "request: {input}");
        assert!(!is_required(&output, "note"), "response: {output}");

        // Still nullable under both. Known gap, pinned deliberately.
        assert_eq!(
            output["properties"]["note"]["type"],
            serde_json::json!(["string", "null"]),
            "schemars 1.2.1 keeps the null branch under the serialize contract \
             even though the server omits the key instead of emitting null — \
             see `self_contained_schema`, \"What it does not fix\": {output}"
        );

        // The narrowing that does work today. `#[schemars(required)]` is not
        // inert: it drops the null branch under both contracts. Combined with
        // the serialize contract it yields the schema this shape actually
        // wants — the key may be absent, but when present it is never null.
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct Narrowed {
            #[serde(skip_serializing_if = "Option::is_none")]
            #[schemars(required)]
            note: Option<String>,
        }
        let narrowed_out = BlockEndpoint::post("/b/x")
            .output::<Narrowed>()
            .output_schema
            .expect("output schema set");
        let narrowed_in = BlockEndpoint::post("/b/x")
            .input::<Narrowed>()
            .input_schema
            .expect("input schema set");

        for (label, schema) in [("response", &narrowed_out), ("request", &narrowed_in)] {
            assert_eq!(
                schema["properties"]["note"]["type"],
                serde_json::json!("string"),
                "{label}: #[schemars(required)] is not inert — it drops the \
                 null branch: {schema}"
            );
        }
        assert!(
            !is_required(&narrowed_out, "note"),
            "the serialize contract still honours skip_serializing_if: the \
             server may omit the key, so the response schema must not demand \
             it even with #[schemars(required)]: {narrowed_out}"
        );
        assert!(
            is_required(&narrowed_in, "note"),
            "the deserialize contract reads #[schemars(required)] as a demand \
             on the sender: {narrowed_in}"
        );
    }

    /// The three request-side builders describe what a client *sends*, so all
    /// three stay on the deserialize contract. Without this, flipping every
    /// builder to serialize would leave the `output_contract_*` tests green
    /// while silently over-constraining every request schema.
    #[cfg(feature = "json-schema")]
    #[test]
    fn only_output_moves_to_the_serialize_contract() {
        let input = BlockEndpoint::post("/b/x")
            .input::<ContractProbe>()
            .input_schema
            .expect("input schema set");
        let path = BlockEndpoint::post("/b/x/{id}")
            .path_params::<ContractProbe>()
            .path_params
            .expect("path params set");
        let query = BlockEndpoint::get("/b/x")
            .query_params::<ContractProbe>()
            .query_params
            .expect("query params set");
        let output = BlockEndpoint::post("/b/x")
            .output::<ContractProbe>()
            .output_schema
            .expect("output schema set");

        assert_eq!(path, input, "path_params must match the request contract");
        assert_eq!(query, input, "query_params must match the request contract");
        assert_ne!(
            output, input,
            "the response contract must differ from the request contract — if \
             these are equal the serialize contract is not being applied: \
             {output}"
        );
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
    /// Inside an OpenAPI document both forms would resolve against the
    /// OpenAPI root rather than the embedded schema.
    /// `wafer_core::discovery::generate_openapi` closes that for both
    /// (`hoist_defs_into_components`): it hoists `$defs` into
    /// `components/schemas` and rewrites `#/$defs/X` to the hoisted entry,
    /// and for a root that references itself it hoists the root too, under
    /// a key taken from its `title`, and rewrites the bare `#` to that
    /// component.
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

        // The field's doc comment survives too — beside the `$ref` the use
        // site became (see `recursive_types_are_referenced_not_unrolled`).
        assert_eq!(
            nested["properties"]["when"]["description"],
            serde_json::json!("The condition tree."),
            "descriptions must survive on recursive fields as well: {nested}"
        );
    }

    /// A recursive type below the root is *referenced*, never unrolled: its
    /// body appears exactly once, in `$defs`, and every use site — the
    /// fields of the root type and the type's own recursive children alike —
    /// is a `$ref` to it. Before this was pinned, schemars' `inline_subschemas`
    /// pasted the full body in at every use site *and* kept the `$defs` copy
    /// that closed the cycle, so a contract using a condition tree in three
    /// places carried its body four times (impresspress's offer tools shipped
    /// ~110 KB of such copies in a 291 KB WebMCP manifest).
    ///
    /// Counting a marker that only the `Equals` variant's doc comment carries
    /// counts bodies directly. The use sites' own doc comments must survive
    /// as siblings of the `$ref`.
    #[cfg(feature = "json-schema")]
    #[test]
    fn recursive_types_are_referenced_not_unrolled() {
        #[derive(schemars::JsonSchema)]
        #[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
        #[allow(dead_code)]
        enum Condition {
            /// Every child condition holds.
            All { all: Vec<Condition> },
            /// The child condition does not hold.
            Not { not: Box<Condition> },
            /// BODY-MARKER: the input equals a literal.
            Equals { equals: String },
        }

        /// A recursive *struct*: an `Option` of it is made nullable by
        /// widening its `type`, not by wrapping it in `anyOf`.
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct Node {
            /// NODE-MARKER: the child nodes.
            children: Vec<Node>,
        }

        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct Rule {
            /// When the rule applies.
            when: Condition,
            /// Conditions that suspend the rule.
            unless: Vec<Condition>,
            /// An optional extra guard.
            guard: Option<Condition>,
            /// An optional tree.
            tree: Option<Node>,
        }

        for (label, schema) in [
            (
                "input",
                BlockEndpoint::post("/b/x").input::<Rule>().input_schema,
            ),
            (
                "output",
                BlockEndpoint::post("/b/x").output::<Rule>().output_schema,
            ),
        ] {
            let schema = schema.expect("schema set");
            let rendered = schema.to_string();
            assert_eq!(
                rendered.matches("BODY-MARKER").count(),
                1,
                "{label}: the `Condition` body must appear exactly once: {rendered}"
            );
            assert!(
                schema["$defs"]["Condition"].is_object(),
                "{label}: the single body lives in `$defs`: {rendered}"
            );

            let when = &schema["properties"]["when"];
            assert_eq!(
                when["$ref"],
                serde_json::json!("#/$defs/Condition"),
                "{label}: {rendered}"
            );
            assert_eq!(
                when["description"],
                serde_json::json!("When the rule applies."),
                "{label}: the use site keeps its own doc next to the `$ref`: {rendered}"
            );
            assert_eq!(
                schema["properties"]["unless"]["description"],
                serde_json::json!("Conditions that suspend the rule."),
                "{label}: {rendered}"
            );
            assert_eq!(
                schema["properties"]["unless"]["items"]["$ref"],
                serde_json::json!("#/$defs/Condition"),
                "{label}: {rendered}"
            );
            assert_eq!(
                schema["properties"]["guard"],
                serde_json::json!({
                    "anyOf": [{ "$ref": "#/$defs/Condition" }, { "type": "null" }],
                    "description": "An optional extra guard."
                }),
                "{label}: {rendered}"
            );

            assert_eq!(
                rendered.matches("NODE-MARKER").count(),
                1,
                "{label}: the `Node` body must appear exactly once: {rendered}"
            );
            assert_eq!(
                schema["properties"]["tree"],
                serde_json::json!({
                    "anyOf": [{ "$ref": "#/$defs/Node" }, { "type": "null" }],
                    "description": "An optional tree."
                }),
                "{label}: a type-widened nullable copy becomes the `anyOf` \
                 form schemars emits for a referenced `Option`: {rendered}"
            );
        }
    }

    /// A recursive type inside another recursive type's body: schemars
    /// inlines `Node` into the `$defs` copy of `Condition` too, so that body
    /// only matches the `Condition` copies elsewhere once its own `Node` copy
    /// has collapsed. The fixpoint over the `$defs` bodies is what makes that
    /// happen before the root is walked.
    #[cfg(feature = "json-schema")]
    #[test]
    fn a_recursive_type_inside_another_is_referenced_too() {
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct Node {
            /// NODE-MARKER: the child nodes.
            children: Vec<Node>,
        }

        #[derive(schemars::JsonSchema)]
        #[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
        #[allow(dead_code)]
        enum Condition {
            /// Every child condition holds.
            All { all: Vec<Condition> },
            /// COND-MARKER: the tree is non-empty.
            Tree { tree: Node },
        }

        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct Rule {
            when: Condition,
            unless: Vec<Condition>,
        }

        let schema = BlockEndpoint::post("/b/x")
            .input::<Rule>()
            .input_schema
            .expect("input schema set");
        let rendered = schema.to_string();
        assert_eq!(rendered.matches("COND-MARKER").count(), 1, "{rendered}");
        assert_eq!(rendered.matches("NODE-MARKER").count(), 1, "{rendered}");
        assert_eq!(
            schema["properties"]["when"],
            serde_json::json!({ "$ref": "#/$defs/Condition" }),
            "{rendered}"
        );
        assert_eq!(
            schema["$defs"]["Condition"]["oneOf"][1]["properties"]["tree"],
            serde_json::json!({ "$ref": "#/$defs/Node" }),
            "the copy inside the other definition collapses too: {rendered}"
        );
    }

    /// A type-widened nullable copy is replaced only when the replacement is
    /// exactly equivalent. A body with a top-level `not` rejects `null` on
    /// its own, so `{"type": ["object", "null"], "not": ...}` does *not*
    /// admit `null` while `anyOf: [{"$ref"}, {"type": "null"}]` would: that
    /// copy must stay inlined.
    #[cfg(feature = "json-schema")]
    #[test]
    fn a_nullable_copy_stays_inlined_when_the_body_can_reject_null() {
        let body = serde_json::json!({
            "type": "object",
            "properties": { "next": { "$ref": "#/$defs/Node" } },
            "not": { "required": ["forbidden"] }
        });
        let mut widened = body.clone();
        widened["type"] = serde_json::json!(["object", "null"]);
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": { "tree": widened.clone() },
            "$defs": { "Node": body }
        });
        reference_recursive_definitions(&mut schema);
        assert_eq!(
            schema["properties"]["tree"], widened,
            "a body with `not` is not null-transparent: {schema}"
        );

        // Without the `not`, the same copy is equivalent and collapses.
        let body = serde_json::json!({
            "type": "object",
            "properties": { "next": { "$ref": "#/$defs/Node" } }
        });
        let mut widened = body.clone();
        widened["type"] = serde_json::json!(["object", "null"]);
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": { "tree": widened },
            "$defs": { "Node": body }
        });
        reference_recursive_definitions(&mut schema);
        assert_eq!(
            schema["properties"]["tree"],
            serde_json::json!({ "anyOf": [{ "$ref": "#/$defs/Node" }, { "type": "null" }] }),
            "{schema}"
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
    fn server_only_round_trips_and_is_absent_when_false() {
        let plain = BlockEndpoint::post("/b/x/y");
        let json = serde_json::to_value(&plain).unwrap();
        assert!(
            json.get("server_only").is_none(),
            "false must not be serialized: {json}"
        );
        // An older guest that never heard of the field still deserializes.
        let old: BlockEndpoint =
            serde_json::from_value(serde_json::json!({"method": "POST", "path": "/b/x/y"}))
                .unwrap();
        assert!(!old.server_only);

        let marked = BlockEndpoint::post("/b/x/y").server_only();
        let back: BlockEndpoint =
            serde_json::from_value(serde_json::to_value(&marked).unwrap()).unwrap();
        assert!(back.server_only);
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

#[cfg(test)]
mod auth_level_order_tests {
    use super::*;

    /// The intended strictness ladder, written out independently of the
    /// derived `Ord`.
    ///
    /// This match is **exhaustive on purpose**: adding a variant to
    /// [`AuthLevel`] stops this file compiling, which forces whoever adds it
    /// to state where on the ladder it belongs. `Ord` is derived from
    /// declaration order, so a variant inserted in the middle of the enum
    /// silently renumbers everything below it — the pair assertions here are
    /// what turn that from a silent security change into a red test.
    fn ladder_position(level: AuthLevel) -> u8 {
        match level {
            AuthLevel::Public => 0,
            AuthLevel::Authenticated => 1,
            AuthLevel::Admin => 2,
        }
    }

    /// Every variant, in the order the ladder claims.
    const LADDER: [AuthLevel; 3] = [
        AuthLevel::Public,
        AuthLevel::Authenticated,
        AuthLevel::Admin,
    ];

    #[test]
    fn derived_order_matches_the_declared_ladder() {
        for a in LADDER {
            for b in LADDER {
                assert_eq!(
                    a.cmp(&b),
                    ladder_position(a).cmp(&ladder_position(b)),
                    "derived Ord disagrees with the strictness ladder for \
                     ({a:?}, {b:?}) — a variant was reordered or inserted \
                     mid-enum"
                );
            }
        }
    }

    /// Pins the ladder itself, not just its agreement with `Ord`: the whole
    /// point of the ordering is that `Public` is the weakest requirement and
    /// `Admin` the strictest, so a caller holding level `L` may see exactly
    /// the endpoints whose requirement is `<= L`.
    #[test]
    fn public_is_weakest_and_admin_is_strictest() {
        assert!(AuthLevel::Public < AuthLevel::Authenticated);
        assert!(AuthLevel::Authenticated < AuthLevel::Admin);
        assert!(AuthLevel::Public < AuthLevel::Admin);
        assert_eq!(LADDER.iter().copied().max(), Some(AuthLevel::Admin));
        assert_eq!(LADDER.iter().copied().min(), Some(AuthLevel::Public));
    }

    /// `Default` must be the bottom of the ladder: an endpoint that declares
    /// no `auth` gets `Public`, and a visibility ceiling built from a
    /// defaulted `AuthLevel` must therefore admit public endpoints only.
    #[test]
    fn default_is_the_bottom_of_the_ladder() {
        assert_eq!(AuthLevel::default(), AuthLevel::Public);
        assert!(LADDER.iter().all(|l| AuthLevel::default() <= *l));
    }
}
