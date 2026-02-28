use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use wafer_run::*;

/// RateLimitBlock provides per-IP rate limiting.
pub struct RateLimitBlock {
    max_requests: u32,
    window: Duration,
    buckets: Mutex<HashMap<String, RateBucket>>,
}

struct RateBucket {
    count: u32,
    window_start: Instant,
}

impl RateLimitBlock {
    pub fn new() -> Self {
        Self {
            max_requests: 1000,
            window: Duration::from_secs(60),
            buckets: Mutex::new(HashMap::new()),
        }
    }
}

impl Block for RateLimitBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/rate-limit".to_string(),
            version: "0.1.0".to_string(),
            interface: "middleware@v1".to_string(),
            summary: "Per-IP rate limiting".to_string(),
            instance_mode: InstanceMode::Singleton,
            allowed_modes: Vec::new(),
            admin_ui: None,
        }
    }

    fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        let max = ctx
            .config_get("max_requests")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(self.max_requests);

        let window_secs = ctx
            .config_get("window_seconds")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(self.window.as_secs());
        let window = Duration::from_secs(window_secs);

        let client_ip = msg.remote_addr().to_string();
        if client_ip.is_empty() {
            return error(
                msg.clone(),
                400,
                "bad_request",
                "Client IP could not be determined",
            );
        }

        let mut buckets = self.buckets.lock();
        let now = Instant::now();

        let bucket = buckets.entry(client_ip).or_insert(RateBucket {
            count: 0,
            window_start: now,
        });

        // Reset window if expired
        if now.duration_since(bucket.window_start) > window {
            bucket.count = 0;
            bucket.window_start = now;
        }

        bucket.count += 1;

        if bucket.count > max {
            let remaining = window
                .checked_sub(now.duration_since(bucket.window_start))
                .unwrap_or(Duration::ZERO);
            let retry_after = remaining.as_secs().to_string();

            let mut m = msg.clone();
            m.set_meta("resp.header.Retry-After", &retry_after);
            m.set_meta(
                "resp.header.X-RateLimit-Limit",
                &max.to_string(),
            );
            m.set_meta("resp.header.X-RateLimit-Remaining", "0");

            return error(m, 429, "rate_limited", "Too many requests");
        }

        let remaining = max - bucket.count;
        msg.set_meta(
            "resp.header.X-RateLimit-Limit",
            &max.to_string(),
        );
        msg.set_meta(
            "resp.header.X-RateLimit-Remaining",
            &remaining.to_string(),
        );

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
    w.register_block("@wafer/rate-limit", Arc::new(RateLimitBlock::new()));
}
