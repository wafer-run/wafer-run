use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;

use crate::compat::{MaybeSend, MaybeSync};
use crate::types::*;

/// ObservabilityContext provides metadata for observability hooks.
#[derive(Clone)]
pub struct ObservabilityContext {
    pub flow_id: String,
    pub node_path: String,
    pub block_name: String,
    pub trace_id: String,
    pub message: Option<Message>,
}

#[cfg(not(target_arch = "wasm32"))]
pub type BlockStartHandler = Arc<dyn Fn(&ObservabilityContext) + Send + Sync>;
#[cfg(target_arch = "wasm32")]
pub type BlockStartHandler = Arc<dyn Fn(&ObservabilityContext)>;

#[cfg(not(target_arch = "wasm32"))]
pub type BlockEndHandler = Arc<dyn Fn(&ObservabilityContext, Duration) + Send + Sync>;
#[cfg(target_arch = "wasm32")]
pub type BlockEndHandler = Arc<dyn Fn(&ObservabilityContext, Duration)>;

#[cfg(not(target_arch = "wasm32"))]
pub type FlowStartHandler = Arc<dyn Fn(&str, &Message) + Send + Sync>;
#[cfg(target_arch = "wasm32")]
pub type FlowStartHandler = Arc<dyn Fn(&str, &Message)>;

#[cfg(not(target_arch = "wasm32"))]
pub type FlowEndHandler = Arc<dyn Fn(&str, Duration) + Send + Sync>;
#[cfg(target_arch = "wasm32")]
pub type FlowEndHandler = Arc<dyn Fn(&str, Duration)>;

/// ObservabilityBus manages multiple observability hook subscribers.
pub struct ObservabilityBus {
    block_start_handlers: RwLock<Vec<BlockStartHandler>>,
    block_end_handlers: RwLock<Vec<BlockEndHandler>>,
    flow_start_handlers: RwLock<Vec<FlowStartHandler>>,
    flow_end_handlers: RwLock<Vec<FlowEndHandler>>,
}

impl ObservabilityBus {
    pub fn new() -> Self {
        Self {
            block_start_handlers: RwLock::new(Vec::new()),
            block_end_handlers: RwLock::new(Vec::new()),
            flow_start_handlers: RwLock::new(Vec::new()),
            flow_end_handlers: RwLock::new(Vec::new()),
        }
    }

    pub fn on_block_start(
        &self,
        h: impl Fn(&ObservabilityContext) + MaybeSend + MaybeSync + 'static,
    ) {
        self.block_start_handlers.write().push(Arc::new(h));
    }

    pub fn on_block_end(
        &self,
        h: impl Fn(&ObservabilityContext, Duration) + MaybeSend + MaybeSync + 'static,
    ) {
        self.block_end_handlers.write().push(Arc::new(h));
    }

    pub fn on_flow_start(&self, h: impl Fn(&str, &Message) + MaybeSend + MaybeSync + 'static) {
        self.flow_start_handlers.write().push(Arc::new(h));
    }

    pub fn on_flow_end(&self, h: impl Fn(&str, Duration) + MaybeSend + MaybeSync + 'static) {
        self.flow_end_handlers.write().push(Arc::new(h));
    }

    pub(crate) fn fire_block_start(&self, ctx: &ObservabilityContext) {
        let handlers = self.block_start_handlers.read();
        for h in handlers.iter() {
            h(ctx);
        }
    }

    pub(crate) fn fire_block_end(&self, ctx: &ObservabilityContext, duration: Duration) {
        let handlers = self.block_end_handlers.read();
        for h in handlers.iter() {
            h(ctx, duration);
        }
    }

    pub(crate) fn fire_flow_start(&self, flow_id: &str, msg: &Message) {
        let handlers = self.flow_start_handlers.read();
        for h in handlers.iter() {
            h(flow_id, msg);
        }
    }

    pub(crate) fn fire_flow_end(&self, flow_id: &str, duration: Duration) {
        let handlers = self.flow_end_handlers.read();
        for h in handlers.iter() {
            h(flow_id, duration);
        }
    }
}

impl Default for ObservabilityBus {
    fn default() -> Self {
        Self::new()
    }
}
