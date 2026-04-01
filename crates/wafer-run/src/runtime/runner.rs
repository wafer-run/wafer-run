use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use futures::FutureExt;

use crate::block::Block;
use crate::config::*;
use crate::observability::ObservabilityContext;
use crate::platform::Instant;
use crate::types::*;

use super::Wafer;

impl Wafer {
    /// Run a flow by ID with the given message.
    pub async fn run(&self, flow_id: &str, msg: &mut Message) -> Result_ {
        let flow = match self.flows.get(flow_id) {
            Some(f) => f,
            None => {
                return Result_ {
                    action: Action::Error,
                    error: Some(WaferError::new(
                        "flow_not_found",
                        format!("flow not found: {}", flow_id),
                    )),
                    response: None,
                    message: None,
                };
            }
        };

        // Observability: flow start
        self.hooks.fire_flow_start(flow_id, msg);
        let start = Instant::now();

        // Set up flow-level timeout via deadline
        let cancelled = Arc::new(AtomicBool::new(false));
        let timeout = flow.config.as_ref().and_then(|c| {
            // Prefer string timeout, fall back to timeout_ms
            if let Some(ref t) = c.timeout {
                let d = parse_duration(t);
                if !d.is_zero() {
                    return Some(d);
                }
            }
            c.timeout_ms.map(std::time::Duration::from_millis)
        });
        let deadline = timeout.and_then(|t| {
            if !t.is_zero() {
                Some(Instant::now() + t)
            } else {
                None
            }
        });

        let result =
            crate::waferflow::execute_waferflow(flow, msg, self, &cancelled, deadline).await;

        // Check timeout
        let result = if deadline.is_some()
            && cancelled.load(Ordering::Relaxed)
            && result.action != Action::Error
        {
            Result_ {
                action: Action::Error,
                error: Some(WaferError::new(
                    "deadline_exceeded",
                    format!("flow {:?} timed out after {:?}", flow_id, timeout),
                )),
                response: None,
                message: result.message,
            }
        } else {
            result
        };

        // Observability: flow end
        self.hooks.fire_flow_end(flow_id, &result, start.elapsed());

        result
    }

    /// Run a single block by name, bypassing flows.
    pub async fn run_block(&self, block_name: &str, msg: &mut Message) -> Result_ {
        // Resolve alias
        let resolved = self
            .aliases
            .get(block_name)
            .map(|s| s.as_str())
            .unwrap_or(block_name);

        let block = match self
            .all_blocks
            .get(resolved)
            .or_else(|| self.all_blocks.get(block_name))
        {
            Some(b) => b.clone(),
            None => {
                return Result_ {
                    action: Action::Error,
                    error: Some(WaferError::new(
                        "block_not_found",
                        format!("block not found: {}", block_name),
                    )),
                    response: None,
                    message: None,
                };
            }
        };

        let cancelled = Arc::new(AtomicBool::new(false));
        let caller_requires = {
            let info = block.info();
            if info.requires.is_empty() {
                None
            } else {
                Some(info.requires)
            }
        };
        let mut ctx = self.make_context(block_name, "root", HashMap::new(), cancelled, None);
        ctx.caller_requires = caller_requires;

        // Observability
        let obs_ctx = ObservabilityContext {
            flow_id: String::new(),
            node_path: "root".to_string(),
            block_name: block_name.to_string(),
            trace_id: msg.get_meta("trace_id").to_string(),
            message: Some(msg.clone()),
        };
        self.hooks.fire_block_start(&obs_ctx);
        let start = Instant::now();

        let result = run_block_with_recovery(&*block, &ctx, msg).await;

        self.hooks
            .fire_block_end(&obs_ctx, &result, start.elapsed());

        result
    }

    /// Flows returns info about all loaded flows.
    pub fn flows_info(&self) -> Vec<wafer_flow::FlowInfo> {
        self.flows
            .values()
            .map(|f| wafer_flow::FlowInfo {
                id: f.id.clone(),
                name: f.name.clone(),
                description: f.description.clone(),
            })
            .collect()
    }

    /// Return all WaferFlow definitions.
    pub fn flow_defs(&self) -> Vec<wafer_flow::WaferFlow> {
        self.flows.values().cloned().collect()
    }
}

/// Execute a block with optional panic recovery.
/// On native: uses catch_unwind to isolate panics.
/// On wasm32: panics abort (handled by Workers runtime).
pub async fn run_block_with_recovery(
    block: &dyn Block,
    ctx: &dyn crate::context::Context,
    msg: &mut Message,
) -> Result_ {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let result = std::panic::AssertUnwindSafe(block.handle(ctx, msg))
            .catch_unwind()
            .await;
        match result {
            Ok(r) => r,
            Err(panic_info) => {
                let panic_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                Result_ {
                    action: Action::Error,
                    error: Some(WaferError::new(
                        "panic",
                        format!("block panicked: {}", panic_msg),
                    )),
                    response: None,
                    message: Some(msg.clone()),
                }
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    {
        block.handle(ctx, msg).await
    }
}
