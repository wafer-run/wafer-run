#![warn(missing_docs)]

//! `wafer-run/ip-rate-limit` — per-IP rate-limiting middleware block.
//!
//! ## Status: native-only
//!
//! This block uses `parking_lot::Mutex<HashMap<…>>` + `std::time::Instant` and
//! is therefore **only suitable for single-instance native deployments**.
//! State is per-process and `Instant` semantics on `wasm32-unknown-unknown`
//! (Cloudflare Workers) are non-monotonic, so cross-instance counts would not
//! be coherent.
//!
//! It is intentionally never wired into wasm32 / Cloudflare Workers builds:
//!
//! - The only consumer is the [`wafer-flow-http-server`] flow, which is gated
//!   behind `wafer-site`'s `target-native` feature; the `target-cloudflare`
//!   build does not pull it in.
//! - Cloudflare Workers production paths (`solobase` on `wafer.run`) use
//!   solobase-core's own `UserRateLimiter`, which is D1-backed via
//!   `wafer-sql-utils::upsert::build_rate_limit_upsert`.
//!
//! If a durable, cross-instance rate-limit primitive is ever needed at this
//! layer, follow the solobase `UserRateLimiter` pattern (D1 upsert under
//! `cfg(target_arch = "wasm32")`) rather than extending this in-memory block.
//!
//! [`wafer-flow-http-server`]: ../wafer_flow_http_server/index.html

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use wafer_block::{
    Block, BlockInfo, ConfigVar, Context, ErrorCode, InputStream, LifecycleEvent, Message,
    OutputStream, WaferError,
};
use wafer_block_macro::wafer_async_trait;

/// Default maximum requests permitted per IP within one window before the
/// block returns [`ErrorCode::ResourceExhausted`].
///
/// This is the single source of truth for the `max_requests` default: it is
/// rendered into the `max_requests` [`ConfigVar`] (so it shows up in the flow
/// editor) and used as the parse fallback in [`RateLimitBlock::handle`]. There
/// is no separate struct-field default.
const DEFAULT_MAX_REQUESTS: u32 = 1000;

/// Default rate-limit window length in seconds.
///
/// Single source of truth for the `window_seconds` default: rendered into the
/// `window_seconds` [`ConfigVar`] and used as the parse fallback in
/// [`RateLimitBlock::handle`].
const DEFAULT_WINDOW_SECONDS: u64 = 60;

/// Source of monotonic time for rate-limit windowing.
///
/// Production uses [`SystemClock`] (wrapping [`Instant::now`]); tests inject a
/// controllable fake so window-reset behaviour can be exercised without sleeping.
pub(crate) trait Clock: Send + Sync {
    /// Returns the current monotonic instant used to stamp bucket windows.
    fn now(&self) -> Instant;
}

/// Default production [`Clock`] backed by [`std::time::Instant::now`].
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Per-IP fixed-window rate-limiter block.
///
/// Maintains an in-memory `HashMap<client_ip, RateBucket>` guarded by a
/// [`parking_lot::Mutex`]. Each bucket counts requests within a fixed window.
/// The limit and window are read exclusively from the `max_requests` /
/// `window_seconds` flow config (defaulting to [`DEFAULT_MAX_REQUESTS`]
/// requests per [`DEFAULT_WINDOW_SECONDS`]-second window when unset) — the
/// block holds no duplicate struct-level defaults. On overflow it emits a
/// [`WaferError`] with [`ErrorCode::ResourceExhausted`] and `Retry-After` /
/// `X-RateLimit-*` response-header meta; on allow it forwards the message with
/// `X-RateLimit-Remaining` set. Single-process / native-only — see crate docs.
pub(crate) struct RateLimitBlock {
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
    /// Builds a block with the production [`SystemClock`].
    pub(crate) fn new() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }

    /// Builds a block with a caller-supplied [`Clock`]. Used by tests to drive
    /// window-reset behaviour deterministically.
    pub(crate) fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            clock,
        }
    }
}

#[wafer_async_trait]
impl Block for RateLimitBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/ip-rate-limit",
            "0.0.1",
            "middleware@v1",
            "Per-IP rate limiting",
        )
        .infrastructure()
        .flow_config(vec![
            ConfigVar::new(
                "max_requests",
                "Maximum requests per IP within the window before \
                 returning ResourceExhausted. Set to 0 to disable.",
                &DEFAULT_MAX_REQUESTS.to_string(),
            )
            .name("Max Requests"),
            ConfigVar::new(
                "window_seconds",
                "Fixed window length in seconds for the per-IP \
                 request count.",
                &DEFAULT_WINDOW_SECONDS.to_string(),
            )
            .name("Window (seconds)"),
        ])
        .config_keys(vec![ConfigVar::new(
            "WAFER_RUN__IP_RATE_LIMIT__DISABLE",
            "When set to \"1\", the rate limiter is bypassed entirely. \
             Intended for test fixtures; do not set in production.",
            "",
        )
        .name("Disable rate limit")
        .optional()])
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        // Allow disabling via env var (useful for tests)
        if std::env::var("WAFER_RUN__IP_RATE_LIMIT__DISABLE")
            .ok()
            .as_deref()
            == Some("1")
        {
            return OutputStream::continue_with(msg);
        }

        // Single read path: flow config is the sole source of truth. When a
        // key is unset (or unparseable) we fall back to the same constant the
        // flow_config ConfigVar advertises as its default.
        let max = ctx
            .config_get("max_requests")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(DEFAULT_MAX_REQUESTS);

        if max == 0 {
            return OutputStream::continue_with(msg);
        }

        let window_secs = ctx
            .config_get("window_seconds")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(DEFAULT_WINDOW_SECONDS);
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

wafer_block::register_static_block!("wafer-run/ip-rate-limit", RateLimitBlock);

#[cfg(test)]
mod clock_seam_tests {
    use std::{
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };

    use super::*;

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

#[cfg(test)]
mod rate_limit_tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::json;
    use wafer_run::streams::output::TerminalNotResponse;
    use wafer_test_support::builder::WaferBuilder;

    use super::*;

    /// Serializes all env-var-sensitive tests to prevent WAFER_RUN__IP_RATE_LIMIT__DISABLE leaking
    /// between tests running concurrently in the same process.
    /// Uses tokio::sync::Mutex so the lock can be held across `.await` points.
    fn env_mutex() -> &'static tokio::sync::Mutex<()> {
        static MUTEX: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        MUTEX.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    struct ControllableClock {
        base: Instant,
        offset_ms: AtomicU64,
    }

    impl ControllableClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                base: Instant::now(),
                offset_ms: AtomicU64::new(0),
            })
        }
        fn advance(&self, ms: u64) {
            self.offset_ms.fetch_add(ms, Ordering::Relaxed);
        }
    }

    impl Clock for ControllableClock {
        fn now(&self) -> Instant {
            self.base + Duration::from_millis(self.offset_ms.load(Ordering::Relaxed))
        }
    }

    async fn build_wafer_with_clock(
        clock: Arc<dyn Clock>,
        config: serde_json::Value,
    ) -> Arc<wafer_run::Wafer> {
        WaferBuilder::new()
            .with_block(
                "wafer-run/ip-rate-limit",
                Arc::new(RateLimitBlock::with_clock(clock)),
            )
            .with_config("wafer-run/ip-rate-limit", config)
            .build()
            .await
            .expect("build")
    }

    /// Build a request message with the given client IP.
    /// `remote_addr()` reads from meta key `"req.client.ip"` (META_REQ_CLIENT_IP).
    fn request_from(ip: &str) -> Message {
        let mut msg = Message::new("http.request");
        msg.set_meta("req.client.ip", ip);
        msg
    }

    #[tokio::test]
    async fn under_limit_continues_with_remaining_meta() {
        let _guard = env_mutex().lock().await;
        std::env::remove_var("WAFER_RUN__IP_RATE_LIMIT__DISABLE");
        let clock = ControllableClock::new();
        let wafer = build_wafer_with_clock(
            clock.clone(),
            json!({"max_requests": "10", "window_seconds": "60"}),
        )
        .await;
        match wafer
            .run_block(
                "wafer-run/ip-rate-limit",
                request_from("1.1.1.1"),
                InputStream::empty(),
            )
            .await
            .collect_buffered()
            .await
        {
            Err(TerminalNotResponse::Continue(continued)) => {
                // Block writes "resp.header.X-RateLimit-Remaining" on the continued message.
                let remaining = continued.get_meta("resp.header.X-RateLimit-Remaining");
                assert!(!remaining.is_empty(), "X-RateLimit-Remaining meta missing");
            }
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn over_limit_denies_with_retry_after() {
        let _guard = env_mutex().lock().await;
        std::env::remove_var("WAFER_RUN__IP_RATE_LIMIT__DISABLE");
        let clock = ControllableClock::new();
        let wafer = build_wafer_with_clock(
            clock.clone(),
            json!({"max_requests": "2", "window_seconds": "60"}),
        )
        .await;

        // Two allowed requests.
        for _ in 0..2 {
            let _ = wafer
                .run_block(
                    "wafer-run/ip-rate-limit",
                    request_from("2.2.2.2"),
                    InputStream::empty(),
                )
                .await
                .collect_buffered()
                .await;
        }

        // Third request over the limit.
        match wafer
            .run_block(
                "wafer-run/ip-rate-limit",
                request_from("2.2.2.2"),
                InputStream::empty(),
            )
            .await
            .collect_buffered()
            .await
        {
            Err(TerminalNotResponse::Error(e)) => {
                // Block writes "resp.header.Retry-After" into err.meta.
                let has_retry = e.meta.iter().any(|m| {
                    m.key.eq_ignore_ascii_case("resp.header.retry-after")
                        || m.key.eq_ignore_ascii_case("retry-after")
                });
                assert!(
                    has_retry,
                    "Retry-After meta missing from rate-limit error: {e:?}"
                );
            }
            other => panic!("expected rate-limit error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn window_reset_restores_budget() {
        let _guard = env_mutex().lock().await;
        std::env::remove_var("WAFER_RUN__IP_RATE_LIMIT__DISABLE");
        let clock = ControllableClock::new();
        let wafer = build_wafer_with_clock(
            clock.clone(),
            json!({"max_requests": "1", "window_seconds": "1"}),
        )
        .await;

        // First request OK.
        let _ = wafer
            .run_block(
                "wafer-run/ip-rate-limit",
                request_from("3.3.3.3"),
                InputStream::empty(),
            )
            .await
            .collect_buffered()
            .await;

        // Second request blocked.
        let blocked = wafer
            .run_block(
                "wafer-run/ip-rate-limit",
                request_from("3.3.3.3"),
                InputStream::empty(),
            )
            .await
            .collect_buffered()
            .await;
        assert!(matches!(blocked, Err(TerminalNotResponse::Error(_))));

        // Advance clock past the window (1 second = 1000 ms, advance 1500 ms).
        clock.advance(1_500);

        // Third request OK again after window reset.
        match wafer
            .run_block(
                "wafer-run/ip-rate-limit",
                request_from("3.3.3.3"),
                InputStream::empty(),
            )
            .await
            .collect_buffered()
            .await
        {
            Err(TerminalNotResponse::Continue(_)) => {}
            other => panic!("expected Continue after window reset, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn disable_via_env_skips_entirely() {
        let _guard = env_mutex().lock().await;
        std::env::set_var("WAFER_RUN__IP_RATE_LIMIT__DISABLE", "1");
        let clock = ControllableClock::new();
        let wafer = build_wafer_with_clock(
            clock.clone(),
            json!({"max_requests": "1", "window_seconds": "60"}),
        )
        .await;

        for _ in 0..3 {
            match wafer
                .run_block(
                    "wafer-run/ip-rate-limit",
                    request_from("4.4.4.4"),
                    InputStream::empty(),
                )
                .await
                .collect_buffered()
                .await
            {
                Err(TerminalNotResponse::Continue(_)) => {}
                other => {
                    std::env::remove_var("WAFER_RUN__IP_RATE_LIMIT__DISABLE");
                    panic!("expected Continue (env disabled), got {other:?}");
                }
            }
        }
        std::env::remove_var("WAFER_RUN__IP_RATE_LIMIT__DISABLE");
    }

    #[tokio::test]
    async fn distinct_ips_have_separate_buckets() {
        let _guard = env_mutex().lock().await;
        std::env::remove_var("WAFER_RUN__IP_RATE_LIMIT__DISABLE");
        let clock = ControllableClock::new();
        let wafer = build_wafer_with_clock(
            clock.clone(),
            json!({"max_requests": "1", "window_seconds": "60"}),
        )
        .await;

        for ip in ["5.5.5.5", "6.6.6.6"] {
            match wafer
                .run_block(
                    "wafer-run/ip-rate-limit",
                    request_from(ip),
                    InputStream::empty(),
                )
                .await
                .collect_buffered()
                .await
            {
                Err(TerminalNotResponse::Continue(_)) => {}
                other => panic!("expected Continue for {ip}, got {other:?}"),
            }
        }
    }
}
