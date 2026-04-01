use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::types::*;

use super::Wafer;

impl Wafer {
    /// Initialize the runtime without calling `bind()` on blocks.
    pub async fn start_without_bind(&mut self) -> Result<(), String> {
        self.resolve().await?;

        // Rebuild the all_blocks map so contexts can see all resolved blocks
        self.rebuild_all_blocks();

        // Snapshot introspection data for contexts
        self.blocks_snapshot = Arc::new(self.blocks.values().map(|b| b.info()).collect());
        self.flow_infos_snapshot = Arc::new(self.flows_info());
        self.flow_defs_snapshot = Arc::new(self.flow_defs());
        self.interface_specs_snapshot = Arc::new(self.interface_specs.values().cloned().collect());

        Ok(())
    }

    /// Start the runtime, wrap in `Arc`, and call `bind()` on all blocks.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn start(mut self) -> Result<Arc<Self>, String> {
        self.start_without_bind().await?;

        let ctx = self.make_context(
            "startup",
            "startup",
            HashMap::new(),
            Arc::new(AtomicBool::new(false)),
            None,
        );
        for (name, block) in &self.blocks {
            if let Err(e) = block
                .lifecycle(
                    &ctx,
                    LifecycleEvent {
                        event_type: LifecycleType::Start,
                        data: Vec::new(),
                    },
                )
                .await
            {
                tracing::error!(block = %name, error = %e, "block start lifecycle failed");
            }
        }

        let arc_self = Arc::new(self);

        let handle = super::RuntimeHandle {
            inner: arc_self.clone(),
        };
        for block in arc_self.blocks.values() {
            block.bind(Box::new(handle.clone()));
        }

        Ok(arc_self)
    }

    /// Shut down all resolved block instances (works through `Arc`).
    pub async fn shutdown(&self) {
        let ctx = self.make_context(
            "shutdown",
            "shutdown",
            HashMap::new(),
            Arc::new(AtomicBool::new(false)),
            None,
        );
        for (name, block) in &self.blocks {
            if let Err(e) = block
                .lifecycle(
                    &ctx,
                    LifecycleEvent {
                        event_type: LifecycleType::Stop,
                        data: Vec::new(),
                    },
                )
                .await
            {
                tracing::error!(block = %name, error = %e, "block stop lifecycle failed");
            }
        }
    }

    /// Stop shuts down all resolved block instances (requires `&mut self`).
    pub async fn stop(&mut self) {
        let ctx = self.make_context(
            "shutdown",
            "shutdown",
            HashMap::new(),
            Arc::new(AtomicBool::new(false)),
            None,
        );
        for (name, block) in &self.blocks {
            if let Err(e) = block
                .lifecycle(
                    &ctx,
                    LifecycleEvent {
                        event_type: LifecycleType::Stop,
                        data: Vec::new(),
                    },
                )
                .await
            {
                tracing::error!(block = %name, error = %e, "block stop lifecycle failed");
            }
        }
    }
}
