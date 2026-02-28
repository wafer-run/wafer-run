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
    error_count: u64,
    status_counts: HashMap<String, u64>,
    path_counts: HashMap<String, u64>,
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
        }
    }

    fn handle(&self, _ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let path = msg.path().to_string();

        // If this is a stats request, return the stats
        if path == "/_stats" || path == "/_monitoring" {
            let stats = self.stats.lock();
            let uptime = self.start_time.elapsed().as_secs();
            return json_respond(
                msg.clone(),
                200,
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
            *stats.path_counts.entry(path).or_insert(0) += 1;
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

pub fn register(w: &mut Wafer) {
    w.register_block("@wafer/monitoring", Arc::new(MonitoringBlock::new()));
}
