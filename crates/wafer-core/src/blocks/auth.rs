use std::sync::Arc;
use wafer_run::*;

/// AuthBlock validates authentication from HTTP request metadata.
/// Supports JWT Bearer tokens, API keys (sb_ prefix), and httpOnly cookies.
pub struct AuthBlock;

impl AuthBlock {
    pub fn new() -> Self {
        Self
    }

    /// Extract auth token from Cookie header or Authorization header.
    fn extract_token(msg: &Message) -> Option<String> {
        // 1. Try httpOnly cookie
        let cookie_token = msg.cookie("auth_token");
        if !cookie_token.is_empty() {
            return Some(cookie_token.to_string());
        }

        // 2. Try Authorization header
        let auth_header = msg.header("Authorization").to_string();
        if auth_header.is_empty() {
            return None;
        }

        if let Some(token) = auth_header.strip_prefix("Bearer ") {
            let token = token.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }

        None
    }

    /// Check if token is an API key (sb_ prefix).
    fn is_api_key(token: &str) -> bool {
        token.starts_with("sb_")
    }

    /// Validate API key against database.
    fn validate_api_key(
        ctx: &dyn Context,
        msg: &mut Message,
        token: &str,
    ) -> std::result::Result<(String, String, Vec<String>), Result_> {
        let services = match ctx.services() {
            Some(s) => s,
            None => return Err(auth_error(msg, 500, "Auth services unavailable")),
        };

        let db = match &services.database {
            Some(db) => db,
            None => return Err(auth_error(msg, 500, "Database service unavailable")),
        };

        let crypto = match &services.crypto {
            Some(c) => c,
            None => return Err(auth_error(msg, 500, "Crypto service unavailable")),
        };

        // Hash the token for lookup
        let key_hash = match crypto.hash(token) {
            Ok(h) => h,
            Err(_) => return Err(auth_error(msg, 500, "Failed to hash API key")),
        };

        // Look up in api_keys table
        let filters = vec![wafer_run::services::database::Filter {
            field: "key_hash".to_string(),
            operator: wafer_run::services::database::FilterOp::Equal,
            value: serde_json::Value::String(key_hash),
        }];

        let opts = wafer_run::services::database::ListOptions {
            filters,
            limit: 1,
            ..Default::default()
        };

        let result = match db.list("api_keys", &opts) {
            Ok(r) => r,
            Err(_) => return Err(auth_error(msg, 401, "Invalid API key")),
        };

        if result.records.is_empty() {
            return Err(auth_error(msg, 401, "Invalid API key"));
        }

        let key_record = &result.records[0];

        // Check if revoked
        if let Some(revoked) = key_record.data.get("revoked_at") {
            if !revoked.is_null() {
                return Err(auth_error(msg, 401, "API key has been revoked"));
            }
        }

        // Check if expired
        if let Some(expires) = key_record.data.get("expires_at") {
            if let Some(expires_str) = expires.as_str() {
                if !expires_str.is_empty() {
                    if let Ok(exp_time) = chrono::DateTime::parse_from_rfc3339(expires_str) {
                        if exp_time < chrono::Utc::now() {
                            return Err(auth_error(msg, 401, "API key has expired"));
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
            return Err(auth_error(msg, 401, "API key has no associated user"));
        }

        // Look up user email
        let email = match db.get("auth_users", &user_id) {
            Ok(user) => user
                .data
                .get("email")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            Err(_) => String::new(),
        };

        // Get user roles from iam_roles table
        let role_filters = vec![wafer_run::services::database::Filter {
            field: "user_id".to_string(),
            operator: wafer_run::services::database::FilterOp::Equal,
            value: serde_json::Value::String(user_id.clone()),
        }];

        let role_opts = wafer_run::services::database::ListOptions {
            filters: role_filters,
            ..Default::default()
        };

        let roles: Vec<String> = match db.list("iam_user_roles", &role_opts) {
            Ok(r) => r
                .records
                .iter()
                .filter_map(|rec| rec.data.get("role").and_then(|v| v.as_str()).map(|s| s.to_string()))
                .collect(),
            Err(_) => Vec::new(),
        };

        Ok((user_id, email, roles))
    }

    /// Validate JWT token.
    fn validate_jwt(
        ctx: &dyn Context,
        msg: &mut Message,
        token: &str,
    ) -> std::result::Result<(String, String, Vec<String>), Result_> {
        let services = match ctx.services() {
            Some(s) => s,
            None => return Err(auth_error(msg, 500, "Auth services unavailable")),
        };

        let crypto = match &services.crypto {
            Some(c) => c,
            None => return Err(auth_error(msg, 500, "Crypto service unavailable")),
        };

        // Verify JWT signature and extract claims
        let claims_map = match crypto.verify(token) {
            Ok(data) => data,
            Err(_) => return Err(auth_error(msg, 401, "Invalid or expired token")),
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
            return Err(auth_error(msg, 401, "Token missing user_id"));
        }

        Ok((user_id, email, roles))
    }
}

impl Block for AuthBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/auth".to_string(),
            version: "0.1.0".to_string(),
            interface: "middleware@v1".to_string(),
            summary: "Authentication middleware: JWT, API key, and cookie auth".to_string(),
            instance_mode: InstanceMode::Singleton,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }

    fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        // Extract token
        let token = match Self::extract_token(msg) {
            Some(t) => t,
            None => return auth_error(msg, 401, "No authentication token provided"),
        };

        // Validate based on token type
        let (user_id, email, roles) = if Self::is_api_key(&token) {
            match Self::validate_api_key(ctx, msg, &token) {
                Ok(v) => v,
                Err(r) => return r,
            }
        } else {
            match Self::validate_jwt(ctx, msg, &token) {
                Ok(v) => v,
                Err(r) => return r,
            }
        };

        // Set auth metadata on the message
        msg.set_meta("auth.user_id", &user_id);
        if !email.is_empty() {
            msg.set_meta("auth.user_email", &email);
        }
        if !roles.is_empty() {
            msg.set_meta("auth.user_roles", &roles.join(","));
        }

        msg.clone().cont()
    }

    fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

fn auth_error(msg: &mut Message, status: u16, message: &str) -> Result_ {
    error(msg.clone(), status, "unauthorized", message)
}

pub fn register(w: &mut Wafer) {
    w.register_block("@wafer/auth", Arc::new(AuthBlock::new()));
}
