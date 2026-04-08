//! Shared message handler logic for the config block.
//!
//! Enforces block-scoped access control:
//! - `SOLOBASE_SHARED__*` keys: any block can read, only admin block can write.
//! - `{ORG}__{BLOCK}__*` keys: only the owning block (or admin) can access.
//! - Unrecognized key format: denied.

use serde::{Deserialize, Serialize};

use wafer_block::common::{ErrorCode, ServiceOp};
use wafer_block::context::Context;
use wafer_block::helpers::{respond_empty, respond_json};
use wafer_block::*;

use super::service::ConfigService;

#[derive(Deserialize)]
struct GetRequest {
    key: String,
}

#[derive(Deserialize)]
struct SetRequest {
    key: String,
    value: String,
}

#[derive(Serialize)]
struct GetResponse {
    value: String,
}

/// Handle a config message by delegating to the given service.
pub fn handle_message(
    service: &dyn ConfigService,
    ctx: &dyn Context,
    msg: &mut Message,
) -> Result_ {
    match msg.kind.as_str() {
        ServiceOp::CONFIG_GET => {
            let key = match msg.decode::<GetRequest>() {
                Ok(req) => req.key,
                Err(_) => {
                    let meta_key = msg.get_meta("key");
                    if meta_key.is_empty() {
                        return Result_::error(WaferError::new(
                            ErrorCode::INVALID_ARGUMENT,
                            "config.get requires a 'key' in data or meta",
                        ));
                    }
                    meta_key.to_string()
                }
            };

            if let Err(e) = check_access(ctx, &key, false) {
                return Result_::error(e);
            }

            match service.get(&key) {
                Some(val) => respond_json(msg, &GetResponse { value: val }),
                None => Result_::error(WaferError::new(
                    ErrorCode::NOT_FOUND,
                    format!("config key not found: {key}"),
                )),
            }
        }
        ServiceOp::CONFIG_SET => {
            let req: SetRequest = match msg.decode() {
                Ok(r) => r,
                Err(e) => {
                    return Result_::error(WaferError::new(
                        ErrorCode::INVALID_ARGUMENT,
                        format!("invalid config.set request: {e}"),
                    ))
                }
            };

            if let Err(e) = check_access(ctx, &req.key, true) {
                return Result_::error(e);
            }

            service.set(&req.key, &req.value);
            respond_empty(msg)
        }
        other => Result_::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown config operation: {other}"),
        )),
    }
}

/// Check whether the calling block is allowed to access the given config key.
///
/// Rules:
/// - `SOLOBASE_SHARED__*`: any block can read; only the admin block can write.
/// - `{ORG}__{BLOCK}__*`: only the owning block or admin can read/write.
/// - Keys without `__` prefix: allowed (legacy/non-namespaced keys during transition).
fn check_access(ctx: &dyn Context, key: &str, is_write: bool) -> Result<(), WaferError> {
    let caller = ctx.caller_id();

    // SOLOBASE_SHARED__ keys: any block can read, only admin can write
    if key.starts_with("SOLOBASE_SHARED__") {
        if is_write {
            match caller {
                Some(c) if c.ends_with("/admin") => return Ok(()),
                _ => {
                    return Err(WaferError::new(
                        ErrorCode::PERMISSION_DENIED,
                        format!(
                            "only admin block can write SOLOBASE_SHARED__ keys (caller: {:?})",
                            caller
                        ),
                    ));
                }
            }
        }
        return Ok(());
    }

    // Block-scoped keys: extract owner from {ORG}__{BLOCK}__{VAR} prefix
    if let Some(owner) = key_owner_block(key) {
        match caller {
            Some(c) if c == owner => Ok(()),            // own key
            Some(c) if c.ends_with("/admin") => Ok(()), // admin can access all
            _ => Err(WaferError::new(
                ErrorCode::PERMISSION_DENIED,
                format!(
                    "block {:?} cannot access key '{}' (owned by '{}')",
                    caller, key, owner
                ),
            )),
        }
    } else {
        // Keys without __ namespacing: allow for backward compat during transition.
        // Once all keys are migrated, this branch can be changed to deny.
        Ok(())
    }
}

/// Extract the owning block name from a namespaced config key.
///
/// `SUPPERS_AI__AUTH__JWT_SECRET` → `suppers-ai/auth`
///
/// Conversion: split on first two `__` to get `{ORG}` and `{BLOCK}`,
/// then lowercase + replace `__` with `/` + replace `_` with `-`.
fn key_owner_block(key: &str) -> Option<String> {
    let first = key.find("__")?;
    let rest = &key[first + 2..];
    let second = rest.find("__")?;
    // prefix = "SUPPERS_AI__AUTH"
    let prefix = &key[..first + 2 + second];
    // Convert: lowercase, __ → /, _ → -
    Some(prefix.to_lowercase().replace("__", "/").replace('_', "-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_owner_block() {
        assert_eq!(
            key_owner_block("SUPPERS_AI__AUTH__JWT_SECRET"),
            Some("suppers-ai/auth".to_string())
        );
        assert_eq!(
            key_owner_block("WAFER_RUN__WEB__STATIC_DIR"),
            Some("wafer-run/web".to_string())
        );
        assert_eq!(
            key_owner_block("SUPPERS_AI__PRODUCTS__STRIPE_SECRET_KEY"),
            Some("suppers-ai/products".to_string())
        );
        // No double-underscore = no owner
        assert_eq!(key_owner_block("JWT_SECRET"), None);
        assert_eq!(key_owner_block("SIMPLE_KEY"), None);
        // SOLOBASE_SHARED__ keys don't have a second __ with a block name
        // but SOLOBASE_SHARED__APP_NAME has only one __ segment before the var
        assert_eq!(key_owner_block("SOLOBASE_SHARED__APP_NAME"), None);
    }
}
