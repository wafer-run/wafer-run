use std::sync::Arc;

use wafer_block::*;
use wafer_core::clients::{
    crypto, database as db,
    database::{Filter, FilterOp, ListOptions},
};

/// AuthBlock validates authentication from HTTP request metadata.
/// Supports JWT Bearer tokens, API keys (sb_ prefix), and httpOnly cookies.
pub struct AuthBlock;

impl Default for AuthBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthBlock {
    pub fn new() -> Self {
        Self
    }

    /// Extract auth token from Authorization header or Cookie.
    /// Bearer tokens take precedence over cookies — explicit credentials
    /// should override ambient browser credentials.
    fn extract_token(msg: &Message) -> Option<String> {
        // 1. Try Authorization header first (explicit > ambient)
        let auth_header = msg.header("Authorization").to_string();
        if !auth_header.is_empty() {
            if let Some(token) = auth_header.strip_prefix("Bearer ") {
                let token = token.trim();
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
        }

        // 2. Fall back to httpOnly cookie
        let cookie_token = msg.cookie("auth_token");
        if !cookie_token.is_empty() {
            return Some(cookie_token.to_string());
        }

        None
    }

    /// Check if token is an API key (sb_ prefix).
    fn is_api_key(token: &str) -> bool {
        token.starts_with("sb_")
    }

    /// Validate API key against database.
    async fn validate_api_key(
        ctx: &dyn Context,
        token: &str,
    ) -> std::result::Result<(String, String, Vec<String>), WaferError> {
        // Use deterministic SHA-256 for key lookup (argon2 is non-deterministic).
        // We use the hash for DB lookup, then do a constant-time comparison
        // of the full hash to prevent timing attacks.
        let key_hash = sha256_hex(token.as_bytes());

        // Look up in api_keys table
        let opts = ListOptions {
            filters: vec![Filter {
                field: "key_hash".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::Value::String(key_hash.clone()),
            }],
            limit: 1,
            ..Default::default()
        };

        let result = match db::list(ctx, "api_keys", &opts).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "failed to look up API key in database");
                return Err(WaferError {
                    code: ErrorCode::Internal,
                    message: "Authentication service unavailable".to_string(),
                    meta: vec![],
                });
            }
        };

        if result.records.is_empty() {
            return Err(WaferError {
                code: ErrorCode::Unauthenticated,
                message: "Invalid API key".to_string(),
                meta: vec![],
            });
        }

        let key_record = &result.records[0];

        // Constant-time comparison of the hash to prevent timing attacks.
        if let Some(stored_hash) = key_record.data.get("key_hash").and_then(|v| v.as_str()) {
            if !constant_time_eq(key_hash.as_bytes(), stored_hash.as_bytes()) {
                return Err(WaferError {
                    code: ErrorCode::Unauthenticated,
                    message: "Invalid API key".to_string(),
                    meta: vec![],
                });
            }
        }

        // Check if revoked
        if let Some(revoked) = key_record.data.get("revoked_at") {
            if !revoked.is_null() {
                return Err(WaferError {
                    code: ErrorCode::Unauthenticated,
                    message: "API key has been revoked".to_string(),
                    meta: vec![],
                });
            }
        }

        // Check if expired
        if let Some(expires) = key_record.data.get("expires_at") {
            if let Some(expires_str) = expires.as_str() {
                if !expires_str.is_empty() {
                    if let Ok(exp_time) = chrono::DateTime::parse_from_rfc3339(expires_str) {
                        if exp_time < chrono::Utc::now() {
                            return Err(WaferError {
                                code: ErrorCode::Unauthenticated,
                                message: "API key has expired".to_string(),
                                meta: vec![],
                            });
                        }
                    }
                }
            }
        }

        // Get user_id from the key
        let user_id = key_record
            .data
            .get("user_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if user_id.is_empty() {
            return Err(WaferError {
                code: ErrorCode::Unauthenticated,
                message: "API key has no associated user".to_string(),
                meta: vec![],
            });
        }

        // Look up user email
        let email = match db::get(ctx, "auth_users", &user_id).await {
            Ok(user) => user
                .data
                .get("email")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            Err(_) => String::new(),
        };

        // Get user roles from iam_roles table
        let role_opts = ListOptions {
            filters: vec![Filter {
                field: "user_id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::Value::String(user_id.clone()),
            }],
            ..Default::default()
        };

        let roles: Vec<String> = match db::list(ctx, "iam_user_roles", &role_opts).await {
            Ok(r) => r
                .records
                .iter()
                .filter_map(|rec| {
                    rec.data
                        .get("role")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .collect(),
            Err(_) => Vec::new(),
        };

        Ok((user_id, email, roles))
    }

    /// Validate JWT token.
    async fn validate_jwt(
        ctx: &dyn Context,
        token: &str,
    ) -> std::result::Result<(String, String, Vec<String>), WaferError> {
        // Verify JWT signature and extract claims
        let Ok(claims_map) = crypto::verify(ctx, token).await else {
            return Err(WaferError {
                code: ErrorCode::Unauthenticated,
                message: "Invalid or expired token".to_string(),
                meta: vec![],
            });
        };

        // Convert claims HashMap to serde_json::Value for uniform access
        let claims = serde_json::Value::Object(
            claims_map
                .into_iter()
                .collect::<serde_json::Map<String, serde_json::Value>>(),
        );

        let user_id = claims
            .get("user_id")
            .or_else(|| claims.get("sub"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let email = claims
            .get("email")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let roles: Vec<String> = if let Some(roles_arr) = claims.get("roles") {
            if let Some(arr) = roles_arr.as_array() {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            } else if let Some(s) = roles_arr.as_str() {
                s.split(',').map(|r| r.trim().to_string()).collect()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        if user_id.is_empty() {
            return Err(WaferError {
                code: ErrorCode::Unauthenticated,
                message: "Token missing user_id".to_string(),
                meta: vec![],
            });
        }

        Ok((user_id, email, roles))
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for AuthBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/auth-validator",
            "0.0.1",
            "middleware@v1",
            "Authentication middleware: JWT, API key, and cookie auth",
        )
        .instance_mode(InstanceMode::Singleton)
        .requires(vec!["wafer-run/crypto".into(), "wafer-run/database".into()])
        .category(BlockCategory::Infrastructure)
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        // Extract token
        let Some(token) = Self::extract_token(&msg) else {
            return OutputStream::error(WaferError {
                code: ErrorCode::Unauthenticated,
                message: "No authentication token provided".to_string(),
                meta: vec![],
            });
        };

        // Validate based on token type
        let (user_id, email, roles) = if Self::is_api_key(&token) {
            match Self::validate_api_key(ctx, &token).await {
                Ok(v) => v,
                Err(e) => return OutputStream::error(e),
            }
        } else {
            match Self::validate_jwt(ctx, &token).await {
                Ok(v) => v,
                Err(e) => return OutputStream::error(e),
            }
        };

        // Set auth metadata on the message and continue
        let mut out_msg = msg;
        out_msg.set_meta("auth.user_id", &user_id);
        if !email.is_empty() {
            out_msg.set_meta("auth.user_email", &email);
        }
        if !roles.is_empty() {
            out_msg.set_meta("auth.user_roles", roles.join(","));
        }

        OutputStream::continue_with(out_msg)
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

/// Constant-time byte comparison to prevent timing side-channel attacks.
/// Iterates over the max length to avoid leaking length information.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..std::cmp::max(a.len(), b.len()) {
        let x = if i < a.len() { a[i] } else { 0 };
        let y = if i < b.len() { b[i] } else { 0 };
        diff |= x ^ y;
    }
    diff == 0
}

pub fn register(w: &mut dyn wafer_block::BlockRegistry) -> Result<(), wafer_block::RuntimeError> {
    w.register_block("wafer-run/auth-validator", Arc::new(AuthBlock::new()))
}
