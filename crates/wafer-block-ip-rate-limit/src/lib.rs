#![warn(missing_docs)]

//! `wafer-run/ip-rate-limit` — per-IP rate-limiting middleware block.
//!
//! ## Status: native-only
//!
//! This block uses in-memory sharded `parking_lot::Mutex<HashMap<…>>` state +
//! `std::time::Instant` and is therefore **only suitable for single-instance
//! native deployments**.
//! State is per-process and `Instant` semantics on `wasm32-unknown-unknown`
//! (Cloudflare Workers) are non-monotonic, so cross-instance counts would not
//! be coherent.
//!
//! It is intentionally never wired into wasm32 / Cloudflare Workers builds:
//!
//! - The only consumer is the [`wafer-flow-http-server`] flow, which is gated
//!   behind `wafer-site`'s `target-native` feature; the `target-cloudflare`
//!   build does not pull it in.
//! - Cloudflare Workers production paths (the consuming application on
//!   `wafer.run`) use the application's own `UserRateLimiter`, which is
//!   D1-backed via the generic windowed-counter builder,
//!   `wafer-sql-utils::upsert::build_windowed_counter_upsert`.
//!
//! If a durable, cross-instance rate-limit primitive is ever needed at this
//! layer, follow that `UserRateLimiter` pattern (D1 upsert under
//! `cfg(target_arch = "wasm32")`) rather than extending this in-memory block.
//!
//! [`wafer-flow-http-server`]: ../wafer_flow_http_server/index.html

use std::{
    collections::HashMap,
    hash::{BuildHasher, RandomState},
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use wafer_block::{
    Block, BlockInfo, ConfigVar, Context, ErrorCode, InputStream, Message, OutputStream, WaferError,
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
/// Maintains an in-memory sharded `HashMap<client_ip, RateBucket>` (see
/// [`ShardedBuckets`]). Each bucket counts requests within a fixed window.
/// The limit and window are read exclusively from the `max_requests` /
/// `window_seconds` flow config (defaulting to [`DEFAULT_MAX_REQUESTS`]
/// requests per [`DEFAULT_WINDOW_SECONDS`]-second window when unset) — the
/// block holds no duplicate struct-level defaults. On overflow it emits a
/// [`WaferError`] with [`ErrorCode::ResourceExhausted`] and `Retry-After` /
/// `X-RateLimit-*` response-header meta; on allow it forwards the message with
/// `X-RateLimit-Remaining` set. Single-process / native-only — see crate docs.
pub(crate) struct RateLimitBlock {
    buckets: ShardedBuckets,
    clock: Arc<dyn Clock>,
}

struct RateBucket {
    count: u32,
    window_start: Instant,
}

/// Number of independent bucket shards. Power of two, sized so that at
/// realistic server concurrency (tens of in-flight requests) two requests for
/// *different* client IPs rarely contend on the same [`Mutex`] (PERF-05 —
/// previously one global mutex serialized every request through the block).
const SHARD_COUNT: usize = 16;

/// Global soft threshold above which expired buckets are swept before
/// inserting. Enforced per shard as `EXPIRY_SWEEP_THRESHOLD / SHARD_COUNT`.
const EXPIRY_SWEEP_THRESHOLD: usize = 1_000;

/// Global hard cap on tracked client buckets. Enforced per shard as
/// `HARD_CAP / SHARD_COUNT`, so the aggregate cap holds exactly when keys
/// hash uniformly and approximately otherwise (the seeded [`RandomState`]
/// keeps an attacker from steering keys into one shard).
const HARD_CAP: usize = 100_000;

/// The client-IP bucket map, split into [`SHARD_COUNT`] independently locked
/// shards so concurrent requests for different IPs don't serialize on one
/// global mutex (PERF-05). A key's shard is chosen by a per-process
/// randomly-seeded hash ([`RandomState`]), which also prevents shard-skew
/// attacks via chosen client IPs. Memory bounds (expiry sweep + hard cap,
/// SEC-10 oldest-first eviction) are enforced per shard with per-shard shares
/// of the global thresholds.
pub(crate) struct ShardedBuckets {
    hasher: RandomState,
    shards: Vec<Mutex<HashMap<String, RateBucket>>>,
}

impl ShardedBuckets {
    fn new() -> Self {
        Self {
            hasher: RandomState::new(),
            shards: (0..SHARD_COUNT)
                .map(|_| Mutex::new(HashMap::new()))
                .collect(),
        }
    }

    fn shard_index(&self, key: &str) -> usize {
        (self.hasher.hash_one(key) as usize) % SHARD_COUNT
    }

    /// Record one request for `key` at `now` under a fixed `window`,
    /// returning the post-increment count and the bucket's window start.
    ///
    /// Locks only `key`'s shard: eviction, window reset, and the increment
    /// all happen under that one shard lock, and the lock is released before
    /// the caller builds its response.
    fn record(&self, key: String, now: Instant, window: Duration) -> (u32, Instant) {
        let mut buckets = self.shards[self.shard_index(&key)].lock();

        // Evict expired entries proactively to prevent unbounded memory growth.
        if buckets.len() > EXPIRY_SWEEP_THRESHOLD / SHARD_COUNT {
            buckets.retain(|_, b| now.duration_since(b.window_start) <= window);
        }
        // Hard cap: if still too large after expiry eviction, drop the oldest
        // ~10% of entries — NOT the whole map (SEC-10). A global clear would
        // reset every active client's counter.
        const SHARD_HARD_CAP: usize = HARD_CAP / SHARD_COUNT;
        if buckets.len() > SHARD_HARD_CAP {
            evict_oldest(&mut buckets, SHARD_HARD_CAP - SHARD_HARD_CAP / 10);
        }

        let bucket = buckets.entry(key).or_insert(RateBucket {
            count: 0,
            window_start: now,
        });

        // Reset window if expired
        if now.duration_since(bucket.window_start) > window {
            bucket.count = 0;
            bucket.window_start = now;
        }

        bucket.count += 1;

        // Copy the results out and release the shard lock before returning
        // (clippy::significant_drop_tightening — and the whole point here is
        // holding the shard lock no longer than necessary).
        let result = (bucket.count, bucket.window_start);
        drop(buckets);
        result
    }
}

/// SEC-10: evict the oldest buckets (by `window_start`) until at most `target`
/// remain. Replaces a global `clear()` that reset *every* active client's
/// counter — a high-cardinality attacker (aided by IP spoofing) could
/// otherwise trip the hard cap repeatedly and wipe all in-flight limits. Here
/// only the least-recently-active buckets are dropped; active clients keep
/// their counts. Work is bounded (one sort) and runs only when the cap is
/// exceeded. Operates on one shard's map (PERF-05 sharding), so the sort cost
/// is also per-shard.
fn evict_oldest(buckets: &mut HashMap<String, RateBucket>, target: usize) {
    if buckets.len() <= target {
        return;
    }
    let remove_count = buckets.len() - target;
    let mut by_age: Vec<(Instant, String)> = buckets
        .iter()
        .map(|(k, b)| (b.window_start, k.clone()))
        .collect();
    // Ascending by window_start: oldest (earliest start) first.
    by_age.sort_unstable_by_key(|(t, _)| *t);
    for (_, k) in by_age.into_iter().take(remove_count) {
        buckets.remove(&k);
    }
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
            buckets: ShardedBuckets::new(),
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

        // record() locks only this IP's shard and releases it before
        // returning, so the response is built lock-free below.
        let now = self.clock.now();
        let (count, window_start) = self.buckets.record(client_ip, now, window);

        if count > max {
            let remaining = window
                .checked_sub(now.duration_since(window_start))
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

        let remaining = max - count;
        let mut out_msg = msg;
        out_msg.set_meta("resp.header.X-RateLimit-Limit", max.to_string());
        out_msg.set_meta("resp.header.X-RateLimit-Remaining", remaining.to_string());

        OutputStream::continue_with(out_msg)
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

    // SEC-10: eviction drops the oldest buckets, not the whole map — active
    // clients keep their counters.
    #[test]
    fn evict_oldest_drops_oldest_and_preserves_recent_counts() {
        let base = Instant::now();
        let mut buckets: std::collections::HashMap<String, RateBucket> =
            std::collections::HashMap::new();
        for i in 0..5u32 {
            buckets.insert(
                format!("ip{i}"),
                RateBucket {
                    count: i + 1,
                    window_start: base + Duration::from_secs(i as u64),
                },
            );
        }
        // Keep only the 2 newest.
        evict_oldest(&mut buckets, 2);
        assert_eq!(buckets.len(), 2);
        assert!(buckets.contains_key("ip4"), "newest survives");
        assert!(buckets.contains_key("ip3"));
        assert!(!buckets.contains_key("ip0"), "oldest dropped");
        assert_eq!(
            buckets.get("ip4").unwrap().count,
            5,
            "a surviving client's counter is NOT reset"
        );
    }

    /// PERF-05: a request must only contend on its own key's shard. With the
    /// pre-shard single global mutex, ANY stalled request blocked EVERY other
    /// request; here a request on a different shard completes even while
    /// another shard's lock is held. Structured as completes-at-all under a
    /// generous timeout — no wall-clock timing asserts.
    #[test]
    fn record_on_a_different_shard_completes_while_another_shard_is_locked() {
        let sb = ShardedBuckets::new();
        let k1 = "10.0.0.1".to_string();
        let s1 = sb.shard_index(&k1);
        // The shard hash is randomly seeded per process, so search for a key
        // on a different shard (256 candidates make a miss astronomically
        // unlikely: P = 16^-256 with 16 uniform shards).
        let k2 = (0..=255u16)
            .map(|i| format!("10.0.1.{i}"))
            .find(|k| sb.shard_index(k) != s1)
            .expect("no candidate key hashed to a different shard");

        let now = Instant::now();
        let window = Duration::from_secs(60);
        std::thread::scope(|scope| {
            // Simulate a request stalled while holding k1's shard lock.
            let stalled_guard = sb.shards[s1].lock();

            let (tx, rx) = std::sync::mpsc::channel();
            let (sb_ref, k2_clone) = (&sb, k2.clone());
            scope.spawn(move || {
                let (count, _) = sb_ref.record(k2_clone, now, window);
                // The main thread only drops `rx` on timeout failure, after
                // which this send result is irrelevant.
                let _ = tx.send(count);
            });

            let count = rx.recv_timeout(Duration::from_secs(10)).expect(
                "record() for a key on a different shard blocked behind an \
                 unrelated shard's lock — sharding regressed to a global mutex",
            );
            assert_eq!(count, 1);
            drop(stalled_guard);
            // scope joins the spawned thread here.
        });
    }

    /// Concurrent records on distinct keys all complete and stay per-key
    /// isolated (each key's first record counts 1) across every shard.
    #[test]
    fn concurrent_records_on_distinct_keys_all_complete_with_isolated_counts() {
        let sb = ShardedBuckets::new();
        let now = Instant::now();
        let window = Duration::from_secs(60);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..32u16)
                .map(|i| {
                    let sb_ref = &sb;
                    scope.spawn(move || sb_ref.record(format!("10.1.0.{i}"), now, window))
                })
                .collect();
            for h in handles {
                let (count, _) = h.join().expect("record thread panicked");
                assert_eq!(count, 1, "each distinct key gets its own bucket");
            }
        });
    }

    /// Repeat records on the SAME key hit the same shard bucket regardless of
    /// which thread records them.
    #[test]
    fn same_key_accumulates_across_threads() {
        let sb = Arc::new(ShardedBuckets::new());
        let now = Instant::now();
        let window = Duration::from_secs(60);
        let mut handles = Vec::new();
        for _ in 0..8 {
            let sb = sb.clone();
            handles.push(std::thread::spawn(move || {
                sb.record("9.9.9.9".to_string(), now, window).0
            }));
        }
        let mut counts: Vec<u32> = handles
            .into_iter()
            .map(|h| h.join().expect("record thread panicked"))
            .collect();
        counts.sort_unstable();
        assert_eq!(counts, (1..=8).collect::<Vec<u32>>());
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
