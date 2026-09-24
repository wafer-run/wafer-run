#![warn(missing_docs)]
//! Read-only guard middleware block for the WAFER runtime.
//!
//! When the `readonly` flow config var is `true`/`1`, this middleware lets
//! through only the read actions — `retrieve`, `list`, and an HTTP `OPTIONS`
//! preflight — and rejects every other action, including `execute` and any
//! custom action, with [`ErrorCode::PermissionDenied`]. The flag accepts
//! exactly `true`/`1`/`false`/`0` (or an empty value, meaning off); anything
//! else fails `lifecycle(Init)` when it is the block's own config, and denies
//! the request when it arrives as a flow step's config.

use wafer_block::*;

/// Middleware block that rejects every non-read action when its flow is
/// configured for read-only mode.
///
/// Registered as `wafer-run/readonly-guard`. Behavior is driven by the
/// `readonly` flow config var; when it is absent the guard is off.
#[derive(Default)]
pub struct ReadonlyGuardBlock;

impl ReadonlyGuardBlock {
    /// Construct the guard. The effective mode is read per request from the
    /// `readonly` config var.
    pub fn new() -> Self {
        Self
    }
}

/// Parse the `readonly` flag. Absent or empty is off; the only other
/// accepted spellings are `true`/`1` and `false`/`0`.
fn parse_readonly(value: Option<&str>) -> Result<bool, WaferError> {
    match value {
        None | Some("" | "false" | "0") => Ok(false),
        Some("true" | "1") => Ok(true),
        Some(other) => Err(WaferError {
            code: ErrorCode::InvalidArgument,
            message: format!(
                "wafer-run/readonly-guard: `readonly` must be true, 1, false or 0, got {other:?}"
            ),
            meta: vec![],
        }),
    }
}

/// Whether `msg` only reads: a `retrieve` or `list` action, or an HTTP
/// `OPTIONS` preflight (which the HTTP codec maps to `execute`). Every other
/// action — including an empty one — may write.
fn is_read(msg: &Message) -> bool {
    let action = msg.action();
    action == RequestAction::RETRIEVE
        || action == "list"
        || (action == RequestAction::EXECUTE
            && msg
                .get_meta(http_codec::META_HTTP_METHOD)
                .eq_ignore_ascii_case("OPTIONS"))
}

#[wafer_async_trait]
impl Block for ReadonlyGuardBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/readonly-guard",
            "0.0.1",
            "middleware@v1",
            "Blocks write operations in read-only mode",
        )
        .infrastructure()
        .flow_config(vec![ConfigVar::new(
            "readonly",
            "When true, the guard admits only retrieve/list actions and \
             OPTIONS preflights, and rejects everything else.",
            "false",
        )
        .name("Read-only")])
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let readonly = match parse_readonly(ctx.config_get("readonly")) {
            Ok(readonly) => readonly,
            // A flag the guard cannot read denies: an operator who typed
            // `yes` meant the guard to be on.
            Err(e) => return OutputStream::error(e),
        };

        if !readonly || is_read(&msg) {
            return OutputStream::continue_with(msg);
        }

        OutputStream::error(WaferError {
            code: ErrorCode::PermissionDenied,
            message: "This instance is in read-only mode. Write operations are not allowed."
                .to_string(),
            meta: vec![],
        })
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init {
            let config = BlockConfig::from_event(&event);
            // The request path reads this key through `parse_config_map`,
            // which stringifies strings, numbers and booleans and drops
            // everything else — so a null, array or object would silently
            // leave the guard off. Refuse those, and check the rest with the
            // same parser `handle` uses.
            match config.as_value().get("readonly") {
                None => {}
                Some(serde_json::Value::String(s)) => {
                    parse_readonly(Some(s))?;
                }
                Some(v @ (serde_json::Value::Bool(_) | serde_json::Value::Number(_))) => {
                    parse_readonly(Some(&v.to_string()))?;
                }
                Some(other) => {
                    return Err(WaferError {
                        code: ErrorCode::InvalidArgument,
                        message: format!(
                            "wafer-run/readonly-guard: `readonly` must be true, 1, false or 0, \
                             got {other}"
                        ),
                        meta: vec![],
                    });
                }
            }
        }
        Ok(())
    }
}

wafer_block::register_static_block!("wafer-run/readonly-guard", ReadonlyGuardBlock);

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use wafer_block::{
        streams::{input::InputStream, output::TerminalNotResponse},
        Message,
    };
    use wafer_test_support::builder::WaferBuilder;

    use super::*;

    async fn build_wafer(config: Option<serde_json::Value>) -> Arc<wafer_run::Wafer> {
        let mut b = WaferBuilder::new().with_block(
            "wafer-run/readonly-guard",
            Arc::new(ReadonlyGuardBlock::new()),
        );
        if let Some(cfg) = config {
            b = b.with_config("wafer-run/readonly-guard", cfg);
        }
        b.build().await.expect("build")
    }

    async fn expect_allowed(wafer: &Arc<wafer_run::Wafer>, action: &str) {
        let mut msg = Message::new(action);
        // Populate META_REQ_ACTION so the action validator picks up the action.
        msg.set_meta("req.action", action);
        match wafer
            .run_block("wafer-run/readonly-guard", msg, InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Ok(_) => {} // Respond terminals are allowed (rare for middleware)
            Err(TerminalNotResponse::Continue(_)) => {} // Expected for middleware
            other => panic!("expected allow for action '{action}', got {other:?}"),
        }
    }

    async fn expect_denied(wafer: &Arc<wafer_run::Wafer>, action: &str) {
        let mut msg = Message::new(action);
        msg.set_meta("req.action", action);
        match wafer
            .run_block("wafer-run/readonly-guard", msg, InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::PermissionDenied);
            }
            other => panic!("expected PermissionDenied for action '{action}', got {other:?}"),
        }
    }

    #[tokio::test]
    async fn readonly_off_write_actions_allowed() {
        let wafer = build_wafer(Some(json!({"readonly": "false"}))).await;
        expect_allowed(&wafer, RequestAction::CREATE).await;
        expect_allowed(&wafer, RequestAction::UPDATE).await;
        expect_allowed(&wafer, RequestAction::DELETE).await;
    }

    #[tokio::test]
    async fn readonly_on_write_actions_all_deny() {
        let wafer = build_wafer(Some(json!({"readonly": "true"}))).await;
        expect_denied(&wafer, RequestAction::CREATE).await;
        expect_denied(&wafer, RequestAction::UPDATE).await;
        expect_denied(&wafer, RequestAction::DELETE).await;
    }

    #[tokio::test]
    async fn readonly_on_read_actions_allowed() {
        let wafer = build_wafer(Some(json!({"readonly": "true"}))).await;
        expect_allowed(&wafer, RequestAction::RETRIEVE).await;
        expect_allowed(&wafer, "list").await;
    }

    #[tokio::test]
    async fn readonly_default_off_allows_writes() {
        let wafer = build_wafer(None).await;
        expect_allowed(&wafer, RequestAction::CREATE).await;
    }

    /// The denylist let everything but create/update/delete through: an
    /// `execute` (an HTTP `POST` to an RPC-style endpoint maps elsewhere, but
    /// `TRACE`, extension methods and non-HTTP callers land here) and any
    /// custom action wrote freely in read-only mode.
    #[tokio::test]
    async fn readonly_on_denies_execute_and_custom_actions() {
        let wafer = build_wafer(Some(json!({"readonly": "true"}))).await;
        expect_denied(&wafer, RequestAction::EXECUTE).await;
        expect_denied(&wafer, "publish").await;
        expect_denied(&wafer, "").await;
    }

    #[tokio::test]
    async fn readonly_on_admits_options_preflight() {
        let wafer = build_wafer(Some(json!({"readonly": "1"}))).await;
        let mut msg = Message::new("OPTIONS:/x");
        msg.set_meta("req.action", RequestAction::EXECUTE);
        msg.set_meta("http.method", "OPTIONS");
        let out = wafer
            .run_block("wafer-run/readonly-guard", msg, InputStream::empty())
            .await
            .collect_buffered()
            .await;
        assert!(
            matches!(out, Err(TerminalNotResponse::Continue(_))),
            "OPTIONS preflight must pass, got {out:?}"
        );
    }

    #[tokio::test]
    async fn readonly_accepts_json_bool_config() {
        let wafer = build_wafer(Some(json!({"readonly": true}))).await;
        expect_denied(&wafer, RequestAction::CREATE).await;
    }

    /// A failed Init is cached on the block's slot and every dispatch to it
    /// answers that error, so even a read never passes an unreadable guard.
    #[tokio::test]
    async fn unknown_flag_value_fails_init() {
        for bad in [json!("yes"), json!("TRUE"), json!(null), json!(["true"])] {
            let wafer = build_wafer(Some(json!({ "readonly": bad }))).await;
            let mut msg = Message::new(RequestAction::RETRIEVE);
            msg.set_meta("req.action", RequestAction::RETRIEVE);
            match wafer
                .run_block("wafer-run/readonly-guard", msg, InputStream::empty())
                .await
                .collect_buffered()
                .await
            {
                Err(TerminalNotResponse::Error(e)) => {
                    assert!(e.message.contains("readonly"), "readonly={bad}: {e:?}");
                }
                other => panic!("readonly={bad} must fail Init, got {other:?}"),
            }
        }
    }

    #[test]
    fn unknown_step_flag_value_denies() {
        let err = parse_readonly(Some("on")).expect_err("`on` is not a flag spelling");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert_eq!(parse_readonly(None).ok(), Some(false));
        assert_eq!(parse_readonly(Some("")).ok(), Some(false));
    }
}
