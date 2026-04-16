use std::{collections::HashMap, sync::Arc, time::Instant};

use parking_lot::Mutex;
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

impl Default for MonitoringBlock {
    fn default() -> Self {
        Self::new()
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
        BlockInfo::new(
            "wafer-run/monitoring",
            "0.0.1",
            "middleware@v1",
            "Request metrics and monitoring",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let path = msg.path().to_string();

        // Stats endpoint — only accessible from loopback addresses.
        // Use an auth middleware in front if broader access control is needed.
        if path == "/_stats" || path == "/_monitoring" {
            let remote = msg.remote_addr().to_string();
            let is_local = remote.is_empty()
                || remote == "127.0.0.1"
                || remote == "::1"
                || remote.starts_with("127.");
            if !is_local {
                return OutputStream::error(WaferError {
                    code: ErrorCode::PermissionDenied,
                    message: "stats endpoint is restricted to localhost".to_string(),
                    meta: vec![],
                });
            }
            let stats = self.stats.lock();
            let uptime = self.start_time.elapsed().as_secs();
            let body = serde_json::to_vec(&serde_json::json!({
                "uptime_seconds": uptime,
                "total_requests": stats.total_requests,
                "error_count": stats.error_count,
                "status_counts": stats.status_counts,
                "top_paths": stats.path_counts,
            }))
            .unwrap_or_default();
            return OutputStream::respond(body);
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

        OutputStream::continue_with(msg)
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

pub fn register(w: &mut Wafer) -> Result<(), RuntimeError> {
    w.register_block("wafer-run/monitoring", Arc::new(MonitoringBlock::new()))
}
