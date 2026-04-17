use std::sync::{atomic::AtomicBool, Arc};

use wafer_block::streams::{input::InputStream, output::OutputStream};

use super::Wafer;
use crate::{
    block::Block, config::*, observability::ObservabilityContext, platform::Instant, types::*,
};

impl Wafer {
    /// Run a flow by ID with the given message.
    pub async fn run(&self, flow_id: &str, msg: Message, input: InputStream) -> OutputStream {
        let Some(flow) = self.flows.get(flow_id) else {
            return OutputStream::error(WaferError::new(
                ErrorCode::NOT_FOUND,
                format!("flow not found: {flow_id}"),
            ));
        };

        // Observability: flow start
        self.hooks.fire_flow_start(flow_id, &msg);
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
            crate::waferflow::execute_waferflow(flow, msg, input, self, &cancelled, deadline).await;

        // Observability: flow end
        self.hooks.fire_flow_end(flow_id, start.elapsed());

        result
    }

    /// Run a single block by name, bypassing flows.
    ///
    /// # Security
    ///
    /// This method bypasses WRAP access control. It is the trusted entry point
    /// for processing external HTTP requests — the HTTP adapter calls this to
    /// dispatch to the first block in the chain.
    ///
    /// `RuntimeHandle` (which exposes this method) must NEVER be passed to
    /// WASM blocks or untrusted code. Native blocks receive it via `bind()`
    /// during lifecycle, which is acceptable because native blocks are trusted
    /// (they run in the same process).
    ///
    /// # Validation
    ///
    /// Top-level dispatch does **not** run the interface-action validator.
    /// That validator only runs on `RuntimeContext::call_block`, which is
    /// the path used when one block calls another. Callers invoking
    /// `run_block` are trusted (e.g., HTTP listeners) and are responsible
    /// for supplying actions the target block can handle.
    pub async fn run_block(
        &self,
        block_name: &str,
        msg: Message,
        input: InputStream,
    ) -> OutputStream {
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
                return OutputStream::error(WaferError::new(
                    ErrorCode::NOT_FOUND,
                    format!("block not found: {block_name}"),
                ));
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

        // Look up block config and flatten to HashMap<String, String>
        let block_config = self
            .block_configs_snapshot
            .get(block_name)
            .map(crate::config::parse_config_map)
            .unwrap_or_default();

        let mut ctx = self.make_context(block_name, "root", block_config, cancelled, None);
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

        let result = block.handle(&ctx, msg, input).await;

        self.hooks.fire_block_end(&obs_ctx, start.elapsed());

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
    msg: Message,
    input: InputStream,
) -> OutputStream {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use futures::FutureExt;
        let result = std::panic::AssertUnwindSafe(block.handle(ctx, msg, input))
            .catch_unwind()
            .await;
        match result {
            Ok(out) => out,
            Err(panic_info) => {
                let panic_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                OutputStream::error(WaferError::new(
                    ErrorCode::INTERNAL,
                    format!("block panicked: {panic_msg}"),
                ))
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    {
        block.handle(ctx, msg, input).await
    }
}
