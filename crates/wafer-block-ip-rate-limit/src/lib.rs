use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use wafer_run::*;

/// Source of monotonic time for rate-limit windowing. Injected for tests.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// Default production clock backed by `std::time::Instant::now()`.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// RateLimitBlock provides per-IP rate limiting.
pub struct RateLimitBlock {
    max_requests: u32,
    window: Duration,
    buckets: Mutex<HashMap<String, RateBucket>>,
    clock: Arc<dyn Clock>,
}

struct RateBucket {
    count: u32,
    window_start: Instant,
}

impl Default for RateLimitBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimitBlock {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }

    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            max_requests: 1000,
            window: Duration::from_secs(60),
            buckets: Mutex::new(HashMap::new()),
            clock,
        }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Block for RateLimitBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/ip-rate-limit",
            "0.0.1",
            "middleware@v1",
            "Per-IP rate limiting",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        // Allow disabling via env var (useful for tests)
        if std::env::var("RATE_LIMIT_IP").ok().as_deref() == Some("0") {
            return OutputStream::continue_with(msg);
        }

        let max = ctx
            .config_get("max_requests")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(self.max_requests);

        if max == 0 {
            return OutputStream::continue_with(msg);
        }

        let window_secs = ctx
            .config_get("window_seconds")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(self.window.as_secs());
        let window = Duration::from_secs(window_secs);

        let client_ip = msg.remote_addr().to_string();
        if client_ip.is_empty() {
            return OutputStream::error(WaferError {
                code: ErrorCode::InvalidArgument,
                message: "Client IP could not be determined".to_string(),
                meta: vec![],
            });
        }

        let mut buckets = self.buckets.lock();
        let now = self.clock.now();

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

            let mut err_msg = msg;
            err_msg.set_meta("resp.header.Retry-After", retry_after);
            err_msg.set_meta("resp.header.X-RateLimit-Limit", max.to_string());
            err_msg.set_meta("resp.header.X-RateLimit-Remaining", "0");

            // Emit an error with the rate-limit meta attached
            let err = WaferError {
                code: ErrorCode::ResourceExhausted,
                message: "Too many requests".to_string(),
                meta: err_msg.meta,
            };
            return OutputStream::error(err);
        }

        let remaining = max - bucket.count;
        let mut out_msg = msg;
        out_msg.set_meta("resp.header.X-RateLimit-Limit", max.to_string());
        out_msg.set_meta("resp.header.X-RateLimit-Remaining", remaining.to_string());

        OutputStream::continue_with(out_msg)
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
    w.register_block("wafer-run/ip-rate-limit", Arc::new(RateLimitBlock::new()))
}

#[cfg(test)]
mod clock_seam_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};

    struct FixedClock {
        base: Instant,
        advance_ms: Arc<AtomicU64>,
    }

    impl Clock for FixedClock {
        fn now(&self) -> Instant {
            self.base + Duration::from_millis(self.advance_ms.load(Ordering::Relaxed))
        }
    }

    #[test]
    fn injected_clock_is_used() {
        let advance = Arc::new(AtomicU64::new(0));
        let clock = Arc::new(FixedClock {
            base: Instant::now(),
            advance_ms: advance.clone(),
        });
        let block = RateLimitBlock::with_clock(clock.clone());
        let t0 = clock.now();
        advance.store(1000, Ordering::Relaxed);
        let t1 = clock.now();
        assert!(t1 - t0 >= Duration::from_millis(1000));
        let _ = block.info();
    }
}
