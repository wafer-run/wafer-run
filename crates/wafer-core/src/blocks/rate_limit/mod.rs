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

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for RateLimitBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo {
            name: "@wafer/ip-rate-limit".to_string(),
            version: "0.1.0".to_string(),
            interface: "middleware@v1".to_string(),
            summary: "Per-IP rate limiting".to_string(),
            instance_mode: InstanceMode::Singleton,
            allowed_modes: Vec::new(),
            admin_ui: None,
            runtime: wafer_run::types::BlockRuntime::Both,
            requires: Vec::new(),
        }
    }

    async fn handle(&self, ctx: &dyn Context, msg: &mut Message) -> Result_ {
        // Allow disabling via env var (useful for tests)
        if std::env::var("RATE_LIMIT_IP").ok().as_deref() == Some("0") {
            return msg.clone().cont();
        }

        let max = ctx
            .config_get("max_requests")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(self.max_requests);

        if max == 0 {
            return msg.clone().cont();
        }

        let window_secs = ctx
            .config_get("window_seconds")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(self.window.as_secs());
        let window = Duration::from_secs(window_secs);

        let client_ip = msg.remote_addr().to_string();
        if client_ip.is_empty() {
            return err_bad_request(msg, "Client IP could not be determined");
        }

        let mut buckets = self.buckets.lock();
        let now = Instant::now();

        // Evict expired entries proactively to prevent unbounded memory growth.
        if buckets.len() > 1_000 {
            buckets.retain(|_, b| now.duration_since(b.window_start) <= window);
        }
        // Hard cap: if still too large after eviction, drop oldest entries
        const HARD_CAP: usize = 100_000;
        if buckets.len() > HARD_CAP {
            buckets.clear();
        }

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

            return error(&m, "resource_exhausted", "Too many requests");
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
    w.register_block("@wafer/ip-rate-limit", Arc::new(RateLimitBlock::new()));
}
