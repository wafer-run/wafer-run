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
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use wafer_block::{
    config::parse_config_map, Block, BlockConfig, BlockInfo, ConfigVar, Context, ErrorCode,
    InputStream, LifecycleEvent, LifecycleType, Message, MetaEntry, OutputStream, WaferError,
};
use wafer_block_macro::wafer_async_trait;

/// Default maximum requests permitted per client within one window before
/// the block returns [`ErrorCode::ResourceExhausted`].
///
/// This is the single source of truth for the `max_requests` default: it is
/// rendered into the `max_requests` [`ConfigVar`] (so it shows up in the flow
/// editor) and used by [`Limits::read`] when the key is unset. There is no
/// separate struct-field default.
const DEFAULT_MAX_REQUESTS: u32 = 1000;

/// Default rate-limit window length in seconds.
///
/// Single source of truth for the `window_seconds` default: rendered into the
/// `window_seconds` [`ConfigVar`] and used by [`Limits::read`] when the key
/// is unset.
const DEFAULT_WINDOW_SECONDS: u64 = 60;

/// Default prefix length one IPv6 client is charged under.
///
/// A /64 is the smallest network an ISP assigns one subscriber, and a host
/// picks any of its 2^64 interface ids itself (SLAAC privacy addresses rotate
/// them routinely), so a bucket per /128 is a fresh budget per request to
/// anyone who wants one. Single source of truth for the `ipv6_prefix`
/// default, like the two above.
const DEFAULT_IPV6_PREFIX: u8 = 64;

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

/// Per-client fixed-window rate-limiter block.
///
/// Maintains an in-memory sharded `HashMap<client network, RateBucket>` (see
/// [`ShardedBuckets`]); [`client_network`] names the network a request is
/// charged to. Each bucket counts requests within a fixed window. The limit,
/// window and IPv6 prefix are read exclusively from the flow config (see
/// [`Limits`]) — the block holds no duplicate struct-level defaults. On
/// overflow it emits a [`WaferError`] with [`ErrorCode::ResourceExhausted`]
/// and `Retry-After` / `X-RateLimit-*` response-header meta; on allow it
/// forwards the message with `X-RateLimit-Remaining` set. Single-process /
/// native-only — see crate docs.
pub(crate) struct RateLimitBlock {
    buckets: ShardedBuckets,
    clock: Arc<dyn Clock>,
}

/// The settings one request is charged under, from the flow config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Limits {
    /// Requests allowed per window; `0` disables the limiter.
    max_requests: u32,
    window: Duration,
    /// Prefix length an IPv6 client is keyed by (`1..=128`).
    ipv6_prefix: u8,
}

impl Limits {
    /// Read the three flow-config keys through `get`. An unset or empty key
    /// takes its default; a present value that does not parse is an
    /// [`ErrorCode::InvalidArgument`] naming the key, never a silent
    /// fallback — an operator who typed `ipv6_prefix = 6O` did not ask for
    /// the default.
    fn read<'a>(get: impl Fn(&str) -> Option<&'a str>) -> Result<Self, WaferError> {
        Ok(Self {
            max_requests: setting(
                get("max_requests"),
                "max_requests",
                DEFAULT_MAX_REQUESTS,
                |_| true,
                "a non-negative integer (0 disables the limiter)",
            )?,
            window: Duration::from_secs(setting(
                get("window_seconds"),
                "window_seconds",
                DEFAULT_WINDOW_SECONDS,
                |secs| *secs > 0,
                "a positive integer number of seconds",
            )?),
            ipv6_prefix: setting(
                get("ipv6_prefix"),
                "ipv6_prefix",
                DEFAULT_IPV6_PREFIX,
                |len| (1..=128).contains(len),
                "an integer prefix length from 1 to 128",
            )?,
        })
    }
}

/// Parse one flow-config value: unset or empty is `default`; otherwise it
/// must parse as `T` and pass `valid`.
fn setting<T: std::str::FromStr>(
    raw: Option<&str>,
    key: &str,
    default: T,
    valid: impl Fn(&T) -> bool,
    expected: &str,
) -> Result<T, WaferError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(default),
        Some(value) => {
            value.parse::<T>().ok().filter(|v| valid(v)).ok_or_else(|| {
                invalid_config(&format!("`{key}` must be {expected}, got {value:?}"))
            })
        }
    }
}

fn invalid_config(detail: &str) -> WaferError {
    WaferError::new(
        ErrorCode::InvalidArgument,
        format!("wafer-run/ip-rate-limit: {detail}"),
    )
}

/// The client network a remote address is charged as, in the one spelling
/// every bucket key uses: an IPv4 address as itself (a /32), an IPv6 address
/// as its `/ipv6_prefix` network (`2001:db8:1:2::/64`), and an IPv4-mapped
/// IPv6 address (`::ffff:a.b.c.d`, what a dual-stack socket reports for an
/// IPv4 peer) as the IPv4 address it carries. A `host:port` form is accepted
/// and the port dropped. `None` when the address does not parse.
fn client_network(remote_addr: &str, ipv6_prefix: u8) -> Option<String> {
    let value = remote_addr.trim();
    let ip = value
        .parse::<IpAddr>()
        .ok()
        .or_else(|| value.parse::<SocketAddr>().ok().map(|addr| addr.ip()))?;
    Some(match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let mask = u128::MAX << (128 - u32::from(ipv6_prefix));
                let network = Ipv6Addr::from(u128::from(v6) & mask);
                format!("{network}/{ipv6_prefix}")
            }
        },
    })
}

struct RateBucket {
    count: u32,
    window_start: Instant,
    /// The window and budget this bucket was last charged under. Two flow
    /// steps may configure different limits, so expiry and "throttled" are
    /// judged per bucket, never by whichever request triggers eviction.
    window: Duration,
    max_requests: u32,
}

impl RateBucket {
    fn expired(&self, now: Instant) -> bool {
        now.duration_since(self.window_start) > self.window
    }

    fn throttled(&self) -> bool {
        self.count >= self.max_requests
    }
}

/// Number of independent bucket shards. Power of two, sized so that at
/// realistic server concurrency (tens of in-flight requests) two requests for
/// *different* clients rarely contend on the same [`Mutex`].
const SHARD_COUNT: usize = 16;

/// Global cap on tracked client buckets. Enforced per shard as
/// `HARD_CAP / SHARD_COUNT`, so the aggregate cap holds exactly when keys
/// hash uniformly and approximately otherwise (the seeded [`RandomState`]
/// keeps an attacker from steering keys into one shard).
const HARD_CAP: usize = 100_000;

/// The client bucket map, split into [`SHARD_COUNT`] independently locked
/// shards so concurrent requests for different clients don't serialize on
/// one global mutex. A key's shard is chosen by a per-process
/// randomly-seeded hash ([`RandomState`]), which also prevents shard-skew
/// attacks via chosen client addresses. The memory bound is enforced per
/// shard by [`evict_for_new_key`].
pub(crate) struct ShardedBuckets {
    hasher: RandomState,
    shards: Vec<Mutex<HashMap<String, RateBucket>>>,
    /// Most buckets one shard holds.
    shard_capacity: usize,
}

impl ShardedBuckets {
    fn new() -> Self {
        Self::with_shard_capacity(HARD_CAP / SHARD_COUNT)
    }

    fn with_shard_capacity(shard_capacity: usize) -> Self {
        Self {
            hasher: RandomState::new(),
            shards: (0..SHARD_COUNT)
                .map(|_| Mutex::new(HashMap::new()))
                .collect(),
            shard_capacity,
        }
    }

    fn shard_index(&self, key: &str) -> usize {
        (self.hasher.hash_one(key) as usize) % SHARD_COUNT
    }

    /// Record one request for `key` at `now` under `limits`, returning the
    /// post-increment count and the bucket's window start.
    ///
    /// Locks only `key`'s shard: eviction, window reset, and the increment
    /// all happen under that one shard lock, and the lock is released before
    /// the caller builds its response. Only a key the shard does not hold yet
    /// can trigger eviction.
    fn record(&self, key: String, now: Instant, limits: Limits) -> (u32, Instant) {
        let mut buckets = self.shards[self.shard_index(&key)].lock();

        if buckets.len() >= self.shard_capacity && !buckets.contains_key(&key) {
            evict_for_new_key(&mut buckets, self.shard_capacity, now);
        }

        let bucket = buckets.entry(key).or_insert(RateBucket {
            count: 0,
            window_start: now,
            window: limits.window,
            max_requests: limits.max_requests,
        });

        // Charged under this request's limits from here on.
        bucket.window = limits.window;
        bucket.max_requests = limits.max_requests;
        if bucket.expired(now) {
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

/// Make room for one new key in a shard holding `capacity` buckets.
///
/// Expired buckets go first: they hold nothing. If that frees nothing, the
/// shard drops live buckets down to 90% of `capacity`, cheapest first:
/// buckets still under their budget before throttled ones, lower counts
/// before higher, older windows before newer. So a flood of fresh keys (one
/// request from each /64 of a /48) evicts its own one-request buckets, and a
/// client that is being throttled keeps its counter; dropping the oldest
/// windows instead would reset exactly the clients closest to their limit.
fn evict_for_new_key(buckets: &mut HashMap<String, RateBucket>, capacity: usize, now: Instant) {
    buckets.retain(|_, b| !b.expired(now));
    if buckets.len() < capacity {
        return;
    }
    let target = capacity - capacity / 10;
    let excess = buckets.len() + 1 - target;
    let mut victims: Vec<(bool, u32, Instant, String)> = buckets
        .iter()
        .map(|(key, b)| (b.throttled(), b.count, b.window_start, key.clone()))
        .collect();
    victims.sort_unstable();
    for (_, _, _, key) in victims.into_iter().take(excess) {
        buckets.remove(&key);
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
                "Maximum requests per client within the window before \
                 returning ResourceExhausted. Set to 0 to disable.",
                &DEFAULT_MAX_REQUESTS.to_string(),
            )
            .name("Max Requests"),
            ConfigVar::new(
                "window_seconds",
                "Fixed window length in seconds for the per-client \
                 request count.",
                &DEFAULT_WINDOW_SECONDS.to_string(),
            )
            .name("Window (seconds)"),
            ConfigVar::new(
                "ipv6_prefix",
                "Prefix length, 1 to 128, that one IPv6 client is counted \
                 under. IPv4 clients are counted per address.",
                &DEFAULT_IPV6_PREFIX.to_string(),
            )
            .name("IPv6 prefix"),
        ])
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let limits = match Limits::read(|key| ctx.config_get(key)) {
            Ok(limits) => limits,
            // A limit the block cannot read denies rather than guessing.
            Err(e) => return OutputStream::error(e),
        };
        if limits.max_requests == 0 {
            return OutputStream::continue_with(msg);
        }

        let Some(client) = client_network(msg.remote_addr(), limits.ipv6_prefix) else {
            return OutputStream::error(WaferError {
                code: ErrorCode::InvalidArgument,
                message: "Client IP could not be determined".to_string(),
                meta: vec![],
            });
        };

        // record() locks only this client's shard and releases it before
        // returning, so the response is built lock-free below.
        let now = self.clock.now();
        let (count, window_start) = self.buckets.record(client, now, limits);
        let max = limits.max_requests;

        if count > max {
            let remaining = limits
                .window
                .checked_sub(now.duration_since(window_start))
                .unwrap_or(Duration::ZERO);
            let retry_after = remaining.as_secs().to_string();

            // The error carries only the rate-limit response headers: an
            // error's meta travels to every transport, so request meta
            // (headers, cookies, caller identity) must never ride on it.
            let err = WaferError {
                code: ErrorCode::ResourceExhausted,
                message: "Too many requests".to_string(),
                meta: vec![
                    MetaEntry {
                        key: "resp.header.Retry-After".to_string(),
                        value: retry_after,
                    },
                    MetaEntry {
                        key: "resp.header.X-RateLimit-Limit".to_string(),
                        value: max.to_string(),
                    },
                    MetaEntry {
                        key: "resp.header.X-RateLimit-Remaining".to_string(),
                        value: "0".to_string(),
                    },
                ],
            };
            return OutputStream::error(err);
        }

        let remaining = max - count;
        let mut out_msg = msg;
        out_msg.set_meta("resp.header.X-RateLimit-Limit", max.to_string());
        out_msg.set_meta("resp.header.X-RateLimit-Remaining", remaining.to_string());

        OutputStream::continue_with(out_msg)
    }

    async fn lifecycle(&self, _ctx: &dyn Context, event: LifecycleEvent) -> Result<(), WaferError> {
        if event.event_type == LifecycleType::Init {
            // The request path sees this config through `parse_config_map`,
            // which stringifies strings, numbers and booleans and drops
            // everything else — so a null, array or object would silently
            // take the default. Refuse those, and check the rest with the
            // parser `handle` uses.
            let config = BlockConfig::from_event(&event);
            for key in ["max_requests", "window_seconds", "ipv6_prefix"] {
                match config.get(key) {
                    None
                    | Some(
                        serde_json::Value::String(_)
                        | serde_json::Value::Number(_)
                        | serde_json::Value::Bool(_),
                    ) => {}
                    Some(other) => {
                        return Err(invalid_config(&format!(
                            "`{key}` must be a number, got {other}"
                        )));
                    }
                }
            }
            let flat = parse_config_map(config.as_value());
            Limits::read(|key| flat.get(key).map(String::as_str))?;
        }
        Ok(())
    }
}

wafer_block::register_static_block!("wafer-run/ip-rate-limit", RateLimitBlock);

#[cfg(test)]
mod bucket_tests {
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

    fn limits(max_requests: u32, window_secs: u64) -> Limits {
        Limits {
            max_requests,
            window: Duration::from_secs(window_secs),
            ipv6_prefix: DEFAULT_IPV6_PREFIX,
        }
    }

    /// `n` distinct keys that hash to `shard` in `sb`.
    fn keys_on_shard(sb: &ShardedBuckets, shard: usize, n: usize) -> Vec<String> {
        (0u32..)
            .map(|i| format!("10.{}.{}.{}", i >> 16, (i >> 8) & 0xff, i & 0xff))
            .filter(|k| sb.shard_index(k) == shard)
            .take(n)
            .collect()
    }

    /// Filling a shard with fresh one-request keys must not reset a client
    /// that is being throttled: eviction takes the cheapest buckets, and a
    /// throttled one is the most expensive to lose. The throttled victim is
    /// also the OLDEST window, which is exactly what oldest-first eviction
    /// dropped.
    #[test]
    fn filling_a_shard_keeps_a_throttled_bucket() {
        let capacity = 10;
        let sb = ShardedBuckets::with_shard_capacity(capacity);
        let base = Instant::now();
        let limit = limits(2, 60);
        let victim = "192.0.2.1".to_string();
        let shard = sb.shard_index(&victim);
        for _ in 0..3 {
            sb.record(victim.clone(), base, limit);
        }

        for (i, key) in keys_on_shard(&sb, shard, 100).into_iter().enumerate() {
            let now = base + Duration::from_millis(i as u64 + 1);
            assert_eq!(sb.record(key, now, limit).0, 1);
        }

        assert!(
            sb.shards[shard].lock().len() <= capacity,
            "shard stays bounded"
        );
        let (count, _) = sb.record(victim, base + Duration::from_secs(1), limit);
        assert_eq!(
            count, 4,
            "the throttled client kept its counter and stays refused"
        );
    }

    /// Eviction judges expiry by each bucket's own window, not by the window
    /// of the request that happens to trigger it: a live one-hour counter is
    /// not dropped by a 60-second request two minutes in.
    #[test]
    fn eviction_judges_expiry_by_each_buckets_own_window() {
        let capacity = 3;
        let sb = ShardedBuckets::with_shard_capacity(capacity);
        let base = Instant::now();
        let hourly = limits(1, 3600);
        let victim = "192.0.2.2".to_string();
        let shard = sb.shard_index(&victim);
        sb.record(victim.clone(), base, hourly);
        sb.record(victim.clone(), base, hourly);

        let later = base + Duration::from_secs(120);
        for key in keys_on_shard(&sb, shard, 4) {
            sb.record(key, later, limits(5, 60));
        }

        let (count, _) = sb.record(victim, later, hourly);
        assert_eq!(count, 3, "the live hourly bucket survived");
    }

    /// Only a key the shard does not hold triggers eviction: a full shard of
    /// live buckets still counts an existing client.
    #[test]
    fn an_existing_key_never_evicts() {
        let sb = ShardedBuckets::with_shard_capacity(2);
        let base = Instant::now();
        let limit = limits(10, 60);
        let first = "192.0.2.3".to_string();
        let shard = sb.shard_index(&first);
        let other = keys_on_shard(&sb, shard, 1).remove(0);
        sb.record(first.clone(), base, limit);
        sb.record(other.clone(), base, limit);
        assert_eq!(sb.record(first, base, limit).0, 2);
        assert_eq!(sb.record(other, base, limit).0, 2);
    }

    #[test]
    fn client_network_spells_each_client_once() {
        let net = |addr: &str| client_network(addr, 64);
        assert_eq!(net("203.0.113.9").as_deref(), Some("203.0.113.9"));
        assert_eq!(net("203.0.113.9:4431").as_deref(), Some("203.0.113.9"));
        assert_eq!(net("::ffff:203.0.113.9").as_deref(), Some("203.0.113.9"));
        assert_eq!(
            net("2001:db8:1:2:aaaa:bbbb:cccc:dddd").as_deref(),
            Some("2001:db8:1:2::/64")
        );
        assert_eq!(
            net("[2001:db8:1:2::1]:443").as_deref(),
            Some("2001:db8:1:2::/64")
        );
        assert_eq!(
            net(" 2001:db8:1:2::1 ").as_deref(),
            Some("2001:db8:1:2::/64")
        );
        assert_eq!(
            client_network("2001:db8:1:2::1", 48).as_deref(),
            Some("2001:db8:1::/48")
        );
        assert_eq!(
            client_network("2001:db8::1", 128).as_deref(),
            Some("2001:db8::1/128")
        );
        for bad in ["", "unknown", "300.1.1.1"] {
            assert_eq!(net(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn limits_take_defaults_and_refuse_bad_values() {
        let none = Limits::read(|_| None).expect("defaults");
        assert_eq!(none, limits(DEFAULT_MAX_REQUESTS, DEFAULT_WINDOW_SECONDS));
        assert_eq!(Limits::read(|_| Some("")).expect("empty is unset"), none);

        for (key, bad) in [
            ("max_requests", "-1"),
            ("max_requests", "ten"),
            ("window_seconds", "0"),
            ("ipv6_prefix", "0"),
            ("ipv6_prefix", "129"),
            ("ipv6_prefix", "6O"),
        ] {
            let err = Limits::read(|k| (k == key).then_some(bad)).expect_err(bad);
            assert_eq!(err.code, ErrorCode::InvalidArgument, "{key}={bad}");
            assert!(err.message.contains(key), "{}", err.message);
        }
    }

    /// A request must only contend on its own key's shard: a request on a
    /// different shard completes even while another shard's lock is held.
    /// Structured as completes-at-all under a generous timeout — no
    /// wall-clock timing asserts.
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
        let limit = limits(10, 60);
        std::thread::scope(|scope| {
            // Simulate a request stalled while holding k1's shard lock.
            let stalled_guard = sb.shards[s1].lock();

            let (tx, rx) = std::sync::mpsc::channel();
            let (sb_ref, k2_clone) = (&sb, k2.clone());
            scope.spawn(move || {
                let (count, _) = sb_ref.record(k2_clone, now, limit);
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
        let limit = limits(10, 60);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..32u16)
                .map(|i| {
                    let sb_ref = &sb;
                    scope.spawn(move || sb_ref.record(format!("10.1.0.{i}"), now, limit))
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
        let limit = limits(10, 60);
        let mut handles = Vec::new();
        for _ in 0..8 {
            let sb = sb.clone();
            handles.push(std::thread::spawn(move || {
                sb.record("9.9.9.9".to_string(), now, limit).0
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
    use std::{
        collections::HashMap,
        sync::atomic::{AtomicU64, Ordering},
    };

    use serde_json::json;
    use wafer_run::{streams::output::TerminalNotResponse, InitError, StaticConfigSource, Wafer};
    use wafer_test_support::builder::WaferBuilder;

    use super::*;

    const BLOCK: &str = "wafer-run/ip-rate-limit";

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
            .with_block(BLOCK, Arc::new(RateLimitBlock::with_clock(clock)))
            .with_config(BLOCK, config)
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

    async fn send(wafer: &wafer_run::Wafer, ip: &str) -> Result<Message, TerminalNotResponse> {
        match wafer
            .run_block(BLOCK, request_from(ip), InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Err(TerminalNotResponse::Continue(msg)) => Ok(msg),
            other => Err(other.expect_err("a middleware never responds")),
        }
    }

    fn is_rate_limited(outcome: &Result<Message, TerminalNotResponse>) -> bool {
        matches!(
            outcome,
            Err(TerminalNotResponse::Error(e)) if e.code == ErrorCode::ResourceExhausted
        )
    }

    #[tokio::test]
    async fn under_limit_continues_with_remaining_meta() {
        let wafer = build_wafer_with_clock(
            ControllableClock::new(),
            json!({"max_requests": "10", "window_seconds": "60"}),
        )
        .await;
        let continued = send(&wafer, "1.1.1.1").await.expect("under the limit");
        assert_eq!(continued.get_meta("resp.header.X-RateLimit-Remaining"), "9");
    }

    #[tokio::test]
    async fn over_limit_denies_with_retry_after() {
        let wafer = build_wafer_with_clock(
            ControllableClock::new(),
            json!({"max_requests": "2", "window_seconds": "60"}),
        )
        .await;

        for _ in 0..2 {
            send(&wafer, "2.2.2.2").await.expect("under the limit");
        }

        match send(&wafer, "2.2.2.2").await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::ResourceExhausted);
                assert!(
                    e.meta.iter().any(|m| m.key == "resp.header.Retry-After"),
                    "Retry-After meta missing from rate-limit error: {e:?}"
                );
            }
            other => panic!("expected rate-limit error, got {other:?}"),
        }
    }

    /// The 429 error carries only its `resp.header.*` entries — never the
    /// request's meta — and so neither the HTTP codec nor the embedder wire
    /// format can surface request headers, cookies or identity from it.
    #[tokio::test]
    async fn over_limit_error_carries_no_request_meta() {
        let wafer = build_wafer_with_clock(
            ControllableClock::new(),
            json!({"max_requests": "1", "window_seconds": "60"}),
        )
        .await;
        let request = || {
            let mut msg = request_from("3.3.3.3");
            msg.set_meta("http.header.authorization", "Bearer SECRET_TOKEN");
            msg.set_meta("http.header.cookie", "session=SECRET_COOKIE");
            msg.set_meta("auth.user_email", "someone@example.com");
            msg
        };
        let _ = wafer
            .run_block(BLOCK, request(), InputStream::empty())
            .await
            .collect_buffered()
            .await;

        match wafer
            .run_block(BLOCK, request(), InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Err(TerminalNotResponse::Error(e)) => {
                let mut keys: Vec<&str> = e.meta.iter().map(|m| m.key.as_str()).collect();
                keys.sort_unstable();
                assert_eq!(
                    keys,
                    vec![
                        "resp.header.Retry-After",
                        "resp.header.X-RateLimit-Limit",
                        "resp.header.X-RateLimit-Remaining",
                    ],
                    "rate-limit error meta must hold only its response headers"
                );
            }
            other => panic!("expected rate-limit error, got {other:?}"),
        }

        // The embedder wire format for the same 429 carries no request meta.
        let json = wafer_run::embed::output_to_json(
            wafer
                .run_block(BLOCK, request(), InputStream::empty())
                .await,
        )
        .await;
        for secret in [
            "SECRET_TOKEN",
            "SECRET_COOKIE",
            "someone@example.com",
            "3.3.3.3",
        ] {
            assert!(
                !json.contains(secret),
                "{secret} leaked into embed JSON: {json}"
            );
        }
    }

    #[tokio::test]
    async fn window_reset_restores_budget() {
        let clock = ControllableClock::new();
        let wafer = build_wafer_with_clock(
            clock.clone(),
            json!({"max_requests": "1", "window_seconds": "1"}),
        )
        .await;

        send(&wafer, "3.3.3.3").await.expect("first request");
        assert!(is_rate_limited(&send(&wafer, "3.3.3.3").await));

        // Advance clock past the window (1 second = 1000 ms, advance 1500 ms).
        clock.advance(1_500);

        send(&wafer, "3.3.3.3")
            .await
            .expect("allowed again after the window resets");
    }

    #[tokio::test]
    async fn distinct_ips_have_separate_buckets() {
        let wafer = build_wafer_with_clock(
            ControllableClock::new(),
            json!({"max_requests": "1", "window_seconds": "60"}),
        )
        .await;

        for ip in ["5.5.5.5", "6.6.6.6"] {
            send(&wafer, ip).await.expect(ip);
        }
    }

    /// A host picks its own interface id, so rotating addresses within its
    /// /64 must not buy a fresh budget. The neighbouring /64 is another
    /// subscriber and keeps its own.
    #[tokio::test]
    async fn an_ipv6_client_is_charged_per_64() {
        let wafer = build_wafer_with_clock(
            ControllableClock::new(),
            json!({"max_requests": "2", "window_seconds": "60"}),
        )
        .await;

        send(&wafer, "2001:db8:1:2::1").await.expect("first");
        send(&wafer, "2001:db8:1:2::2").await.expect("second");
        assert!(
            is_rate_limited(&send(&wafer, "2001:db8:1:2:ffff:ffff:ffff:ffff").await),
            "a third address in the same /64 shares the spent budget"
        );
        send(&wafer, "2001:db8:1:3::1")
            .await
            .expect("the neighbouring /64 has its own budget");
    }

    /// A dual-stack socket reports an IPv4 peer as `::ffff:a.b.c.d`; that is
    /// the same client as the plain IPv4 address.
    #[tokio::test]
    async fn an_ipv4_mapped_address_is_charged_as_ipv4() {
        let wafer = build_wafer_with_clock(
            ControllableClock::new(),
            json!({"max_requests": "1", "window_seconds": "60"}),
        )
        .await;

        send(&wafer, "198.51.100.7").await.expect("first");
        assert!(is_rate_limited(&send(&wafer, "::ffff:198.51.100.7").await));
    }

    /// `ipv6_prefix` is the flow's to choose: at 128 every address is its
    /// own client again.
    #[tokio::test]
    async fn ipv6_prefix_is_configurable() {
        let wafer = build_wafer_with_clock(
            ControllableClock::new(),
            json!({"max_requests": "1", "window_seconds": "60", "ipv6_prefix": "128"}),
        )
        .await;

        send(&wafer, "2001:db8:1:2::1").await.expect("first");
        send(&wafer, "2001:db8:1:2::2")
            .await
            .expect("a /128 prefix gives each address its own budget");
    }

    /// A step's own config, for driving `handle` with a per-call config the
    /// registered block config does not have.
    #[derive(Clone)]
    struct StepConfig(HashMap<String, String>);

    #[async_trait::async_trait]
    impl Context for StepConfig {
        async fn call_block(
            &self,
            block: &str,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            panic!("the rate limiter calls no block, got {block}");
        }
        fn is_cancelled(&self) -> bool {
            false
        }
        fn config_get(&self, key: &str) -> Option<&str> {
            self.0.get(key).map(String::as_str)
        }
        fn clone_arc(&self) -> Arc<dyn Context> {
            Arc::new(self.clone())
        }
        fn resource_access_admitted(
            &self,
            _resource: &str,
            _resource_type: wafer_block::types::ResourceType,
            _access: wafer_block::types::ResourceAccess,
        ) -> bool {
            false
        }
    }

    /// A step limit the block cannot read denies the request rather than
    /// running with a default the operator did not choose.
    #[tokio::test]
    async fn an_unreadable_step_limit_denies() {
        let ctx = StepConfig(HashMap::from([(
            "ipv6_prefix".to_string(),
            "sixty-four".to_string(),
        )]));
        let out = RateLimitBlock::new()
            .handle(&ctx, request_from("2001:db8::1"), InputStream::empty())
            .await
            .collect_buffered()
            .await;
        match out {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(e.message.contains("ipv6_prefix"), "{}", e.message);
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    /// A malformed registered config fails Init, naming the key, instead of
    /// the request path silently falling back to a default.
    #[tokio::test]
    async fn a_malformed_block_config_fails_init() {
        for config in [
            json!({"ipv6_prefix": "0"}),
            json!({"window_seconds": {"secs": 60}}),
            json!({"max_requests": "lots"}),
        ] {
            let mut wafer = Wafer::builder()
                .disable_inventory()
                .disable_lockfile()
                .build()
                .expect("build");
            wafer
                .register_block(BLOCK, Arc::new(RateLimitBlock::new()))
                .expect("register");
            wafer.add_block_config(BLOCK, config.clone());
            let wafer = wafer.start().await.expect("start");
            match wafer.init_block(BLOCK).await {
                Err(InitError::Permanent(message)) => {
                    assert!(message.contains("ip-rate-limit"), "{config}: {message}");
                }
                other => panic!("{config}: expected a permanent Init failure, got {other:?}"),
            }
        }
    }

    /// The block declares no process-level config: its one switch was an
    /// env-only `DISABLE` flag that no `ConfigSource` could reach. Turning
    /// the limiter off is `max_requests = 0`, in the flow config.
    #[tokio::test]
    async fn max_requests_zero_disables_and_no_env_switch_is_declared() {
        assert!(RateLimitBlock::new().info().config_keys.is_empty());

        let mut wafer = Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .config_source(Arc::new(StaticConfigSource::default()))
            .build()
            .expect("build");
        wafer
            .register_block(BLOCK, Arc::new(RateLimitBlock::new()))
            .expect("register");
        wafer.add_block_config(BLOCK, json!({"max_requests": 0}));
        let wafer = wafer.start().await.expect("start");
        for _ in 0..3 {
            send(&wafer, "4.4.4.4").await.expect("disabled limiter");
        }
    }
}
