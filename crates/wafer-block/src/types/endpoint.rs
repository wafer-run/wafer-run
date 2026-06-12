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
