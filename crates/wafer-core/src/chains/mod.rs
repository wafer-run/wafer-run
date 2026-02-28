use wafer_run::ChainDef;

/// Create the standard HTTP infrastructure chain.
/// Applies security headers, CORS, readonly guard, rate limiting, and monitoring.
pub fn http_infra_chain() -> Result<ChainDef, String> {
    serde_json::from_str(HTTP_INFRA_JSON)
        .map_err(|e| format!("invalid http-infra chain JSON: {}", e))
}

/// Create the auth pipeline chain.
pub fn auth_pipe_chain() -> Result<ChainDef, String> {
    serde_json::from_str(AUTH_PIPE_JSON)
        .map_err(|e| format!("invalid auth-pipe chain JSON: {}", e))
}

const HTTP_INFRA_JSON: &str = r#"{
    "id": "http-infra",
    "summary": "Standard HTTP infrastructure: security headers, CORS, rate limiting, monitoring",
    "config": { "on_error": "stop" },
    "root": {
        "block": "@wafer/security-headers",
        "next": [
            {
                "block": "@wafer/cors",
                "next": [
                    {
                        "block": "@wafer/readonly-guard",
                        "next": [
                            {
                                "block": "@wafer/rate-limit",
                                "next": [
                                    {
                                        "block": "@wafer/monitoring"
                                    }
                                ]
                            }
                        ]
                    }
                ]
            }
        ]
    }
}"#;

const AUTH_PIPE_JSON: &str = r#"{
    "id": "auth-pipe",
    "summary": "Authentication pipeline: infra + auth check",
    "config": { "on_error": "stop" },
    "root": {
        "chain": "http-infra",
        "next": [
            {
                "block": "@wafer/auth"
            }
        ]
    }
}"#;

/// Create the admin pipeline chain.
/// Requires admin authentication (auth + IAM with role=admin).
/// Includes http-infra for security headers, CORS, rate limiting, and monitoring.
pub fn admin_pipe_chain() -> Result<ChainDef, String> {
    serde_json::from_str(ADMIN_PIPE_JSON)
        .map_err(|e| format!("invalid admin-pipe chain JSON: {}", e))
}

const ADMIN_PIPE_JSON: &str = r#"{
    "id": "admin-pipe",
    "summary": "Admin pipeline: infra + auth + IAM admin role check",
    "config": { "on_error": "stop" },
    "root": {
        "chain": "http-infra",
        "next": [
            {
                "block": "@wafer/auth",
                "next": [
                    {
                        "block": "@wafer/iam",
                        "config": { "role": "admin" }
                    }
                ]
            }
        ]
    }
}"#;

/// Register the standard chain templates with a Wafer runtime.
pub fn register_chains(w: &mut wafer_run::Wafer) -> Result<(), String> {
    w.add_chain_def(&http_infra_chain()?);
    w.add_chain_def(&auth_pipe_chain()?);
    w.add_chain_def(&admin_pipe_chain()?);
    Ok(())
}
