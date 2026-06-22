//! Request metrics and a loopback-only stats endpoint for the WAFER runtime.
//!
//! Registers the `wafer-run/monitoring` block, a middleware that counts
//! requests / paths / response statuses and exposes a JSON snapshot at
//! `/_stats` (alias `/_monitoring`). The endpoint paths are configurable
//! via the `stats_path` / `monitoring_path` keys (block-level Init config
//! or per-request `ctx.config_get`).
//!
//! The stats payload is hand-rolled JSON (not Prometheus / OpenTelemetry)
//! and only served to clients whose source IP parses as a loopback address
//! (127.0.0.0/8, `::1`, or `::ffff:127.x.y.z`). Front it with an auth
//! middleware if you need broader access.

#![warn(missing_docs)]

use std::{collections::HashMap, net::IpAddr, sync::OnceLock, time::Instant};

use parking_lot::Mutex;
use wafer_block::{
    Block, BlockConfig, BlockInfo, ConfigVar, Context, ErrorCode, InputStream, LifecycleEvent,
    LifecycleType, Message, OutputStream, WaferError,
};
use wafer_block_macro::wafer_async_trait;

/// Default routes for the stats / monitoring endpoints. Overridable via
/// the `stats_path` / `monitoring_path` flow_config keys (per request)
/// or via the block-level config JSON passed to `lifecycle(Init)`.
const DEFAULT_STATS_PATH: &str = "/_stats";
const DEFAULT_MONITORING_PATH: &str = "/_monitoring";

/// Middleware block that tracks per-request metrics and serves the JSON
/// stats endpoint. Singleton per Wafer instance; auto-registered via
/// [`wafer_block::register_static_block!`].
pub(crate) struct MonitoringBlock {
    start_time: Instant,
    stats: Mutex<MonitoringStats>,
    /// Init-cached endpoint paths (write-once). `handle()` prefers per-request
    /// `ctx.config_get` and falls back to these, or to
    /// [`EndpointPaths::default`] when Init supplied no config.
    paths: OnceLock<EndpointPaths>,
}

#[derive(Clone)]
struct EndpointPaths {
    stats: String,
    monitoring: String,
}

impl Default for EndpointPaths {
    fn default() -> Self {
        Self {
            stats: DEFAULT_STATS_PATH.into(),
            monitoring: DEFAULT_MONITORING_PATH.into(),
        }
    }
}

struct MonitoringStats {
    total_requests: u64,
    path_counts: HashMap<String, u64>,
}

impl Default for MonitoringBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl MonitoringBlock {
    /// Construct a fresh monitoring block with the uptime clock started
    /// at `now` and all counters zeroed.
    pub(crate) fn new() -> Self {
        Self {
            start_time: Instant::now(),
            stats: Mutex::new(MonitoringStats {
                total_requests: 0,
                path_counts: HashMap::new(),
            }),
            paths: OnceLock::new(),
        }
    }

    /// Resolve `(stats_path, monitoring_path)` from the per-request
    /// context (preferred) or the Init-cached paths (or their defaults when
    /// Init supplied no config).
    fn resolved_paths(&self, ctx: &dyn Context) -> (String, String) {
        let cached = self.paths.get().cloned().unwrap_or_default();
        let stats = ctx
            .config_get("stats_path")
            .map(|s| s.to_string())
            .unwrap_or(cached.stats);
        let monitoring = ctx
            .config_get("monitoring_path")
            .map(|s| s.to_string())
            .unwrap_or(cached.monitoring);
        (stats, monitoring)
    }
}

/// Returns true when `addr` parses as a loopback IP — 127.0.0.0/8
/// (IPv4), `::1` (IPv6), or an IPv4-mapped IPv6 like `::ffff:127.0.0.1`.
///
/// Dual-stack listening sockets routinely deliver IPv4 loopback
/// connections as `::ffff:127.x.y.z`, so the mapped-IPv6 case must be
/// unwrapped before [`Ipv4Addr::is_loopback`] sees it — `IpAddr::is_loopback`
/// alone says `false` for those.
///
/// Falsy for empty / unparseable strings — callers handle the empty case
/// separately where it's meaningful (e.g. internal non-HTTP callers
/// don't populate `req.client.ip`).
fn is_loopback_addr(addr: &str) -> bool {
    match addr.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.is_loopback(),
        Ok(IpAddr::V6(v6)) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
        Err(_) => false,
    }
}

#[wafer_async_trait]
impl Block for MonitoringBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/monitoring",
            "0.0.1",
            "middleware@v1",
            "Request metrics and monitoring",
        )
        .infrastructure()
        .flow_config(vec![
            ConfigVar::new(
                "stats_path",
                "URL path that serves the JSON stats blob \
                 (uptime / counts). Loopback-only.",
                DEFAULT_STATS_PATH,
            )
            .name("Stats Path"),
            ConfigVar::new(
                "monitoring_path",
                "Alias path for the stats endpoint. Same access \
                 rules, same response shape.",
                DEFAULT_MONITORING_PATH,
            )
            .name("Monitoring Path"),
        ])
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let path = msg.path().to_string();
        let (stats_path, monitoring_path) = self.resolved_paths(ctx);

        // Stats endpoint — only accessible from loopback addresses.
        // Use an auth middleware in front if broader access control is needed.
        if path == stats_path || path == monitoring_path {
            let remote = msg.remote_addr();
            // Treat an empty `req.client.ip` as trusted (internal / non-HTTP
            // callers don't populate the meta key). Everything else must
            // parse as an actual loopback IP — `127.foo`-style spoofing
            // doesn't survive `IpAddr::parse`.
            let is_local = remote.is_empty() || is_loopback_addr(remote);
            if !is_local {
                return OutputStream::error(WaferError {
                    code: ErrorCode::PermissionDenied,
                    message: "stats endpoint is restricted to localhost".to_string(),
                    meta: vec![],
                });
            }
            // Snapshot the stats fields under the lock, drop the lock,
            // then serialise. Holding `parking_lot::Mutex` across a
            // potentially-long `serde_json::to_vec` blocks every other
            // request that touches the counters.
            let uptime = self.start_time.elapsed().as_secs();
            let payload = {
                let stats = self.stats.lock();
                serde_json::json!({
                    "uptime_seconds": uptime,
                    "total_requests": stats.total_requests,
                    "top_paths": stats.path_counts,
                })
            };
            // Serialize via the shared `response::ok_json` helper: it sets
            // `Content-Type: application/json` and, crucially, turns a
            // serialization failure into a logged `Internal` error rather than
            // the old `.unwrap_or_default()` which silently collapsed any
            // failure into an empty HTTP 200. The sibling inspector block uses
            // the same helper, so the two no longer drift on error handling.
            return wafer_block::response::ok_json(&payload);
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
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        if event.event_type == LifecycleType::Init {
            let config = BlockConfig::from_event(&event);
            let paths = EndpointPaths {
                stats: config.str_or("stats_path", DEFAULT_STATS_PATH).to_string(),
                monitoring: config
                    .str_or("monitoring_path", DEFAULT_MONITORING_PATH)
                    .to_string(),
            };
            // Write-once: Init fires a single time per registration.
            let _ = self.paths.set(paths);
        }
        Ok(())
    }
}

wafer_block::register_static_block!("wafer-run/monitoring", MonitoringBlock);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_ipv4_accepted() {
        assert!(is_loopback_addr("127.0.0.1"));
        // RFC 1122: every 127.0.0.0/8 address is loopback.
        assert!(is_loopback_addr("127.255.255.255"));
        assert!(is_loopback_addr("127.0.1.2"));
    }

    #[test]
    fn loopback_ipv6_accepted() {
        assert!(is_loopback_addr("::1"));
        // IPv4-mapped IPv6 loopback.
        assert!(is_loopback_addr("::ffff:127.0.0.1"));
    }

    #[test]
    fn non_loopback_rejected() {
        assert!(!is_loopback_addr("10.0.0.1"));
        assert!(!is_loopback_addr("192.168.1.1"));
        assert!(!is_loopback_addr("128.0.0.1"));
        assert!(!is_loopback_addr("0.0.0.0"));
        assert!(!is_loopback_addr("2001:db8::1"));
    }

    #[test]
    fn spoof_attempts_rejected() {
        // The old `starts_with("127.")` check admitted these. `IpAddr::parse` doesn't.
        assert!(!is_loopback_addr("127.foo"));
        assert!(!is_loopback_addr("127.0.0.1.evil.com"));
        assert!(!is_loopback_addr("127."));
        assert!(!is_loopback_addr(""));
        assert!(!is_loopback_addr("not-an-ip"));
    }
}
