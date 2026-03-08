use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use wafer_run::*;

/// MonitoringBlock tracks request metrics and provides a stats endpoint.
pub struct MonitoringBlock {
    start_time: Instant,
    stats: Mutex<MonitoringStats>,
}

struct MonitoringStats {
    total_requests: u64,
    // NOTE: error_count is not automatically incremented because this middleware
    // runs before downstream handlers. Call `increment_error()` externally or
    // check response status in a post-processing step.
    error_count: u64,
    status_counts: HashMap<String, u64>,
    path_counts: HashMap<String, u64>,
}

impl MonitoringBlock {
    /// Increment the error count. Call from post-processing when a response
    /// indicates an error (e.g., HTTP 5xx status).
    pub fn record_error(&self) {
        self.stats.lock().error_count += 1;
    }

    /// Record a response status code for metrics tracking.
    pub fn record_status(&self, status: &str) {
        let mut stats = self.stats.lock();
        *stats.status_counts.entry(status.to_string()).or_insert(0) += 1;
    }
}

impl MonitoringBlock {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            stats: Mutex::new(MonitoringStats {
                total_requests: 0,
                error_count: 0,
                status_counts: HashMap::new(),
                path_counts: HashMap::new(),
            }),
        }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for MonitoringBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/monitoring".to_string(),
            version: "0.1.0".to_string(),
            interface: "middleware@v1".to_string(),
            summary: "Request metrics and monitoring".to_string(),
            instance_mode: InstanceMode::Singleton,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: wafer_run::types::BlockRuntime::Wasm,
            requires: Vec::new(),
        }
    }

    async fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let path = msg.path().to_string();

        // Stats endpoint — only accessible from loopback addresses.
        // Use an auth middleware in front if broader access control is needed.
        if path == "/_stats" || path == "/_monitoring" {
            let remote = msg.remote_addr();
            let is_local = remote.is_empty()
                || remote == "127.0.0.1"
                || remote == "::1"
                || remote.starts_with("127.");
            if !is_local {
                return err_forbidden(msg, "stats endpoint is restricted to localhost");
            }
            let stats = self.stats.lock();
            let uptime = self.start_time.elapsed().as_secs();
            return json_respond(
                msg,
                &serde_json::json!({
                    "uptime_seconds": uptime,
                    "total_requests": stats.total_requests,
                    "error_count": stats.error_count,
                    "status_counts": stats.status_counts,
                    "top_paths": stats.path_counts,
                }),
            );
        }

        // Track the request
        {
            let mut stats = self.stats.lock();
            stats.total_requests += 1;
            // Cap path_counts to prevent unbounded memory growth from path-scanning attacks
            if stats.path_counts.len() < 10_000 || stats.path_counts.contains_key(&path) {
                *stats.path_counts.entry(path).or_insert(0) += 1;
            }
        }

        msg.clone().cont()
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn register(w: &mut Wafer) {
    w.register_block("@wafer/monitoring", Arc::new(MonitoringBlock::new()));
}
