//! Request metrics and an access-controlled stats endpoint for the WAFER
//! runtime.
//!
//! Registers the `wafer-run/monitoring` block, a middleware that counts
//! requests per declared route and exposes a JSON snapshot at `/_stats`
//! (alias `/_monitoring`). The endpoint paths are configurable via the
//! `stats_path` / `monitoring_path` keys (block-level Init config or
//! per-request `ctx.config_get`).
//!
//! Requests are counted under the route *template* they match — a
//! `BlockEndpoint::path` some registered block declares, such as
//! `/b/auth/reset/{token}` — never under the raw request path, which can
//! carry reset tokens and share links. A path no block declares is counted
//! under [`UNMATCHED_ROUTE`].
//!
//! The stats payload is hand-rolled JSON (not Prometheus / OpenTelemetry).
//! Who may read it is the `stats_access` setting:
//! - `roles` (the default): the caller's `auth.user_roles` must contain one
//!   of `stats_roles` (default `admin`), so an auth middleware has to run
//!   before this block in the flow — without one, nobody can read it;
//! - `loopback`: the caller's `req.client.ip` must parse as a loopback
//!   address (127.0.0.0/8, `::1`, or `::ffff:127.x.y.z`); an empty or
//!   unparseable address is refused. Behind a reverse proxy on the same host
//!   every request arrives from loopback unless the listener resolves the
//!   real client from its trusted proxies, so this mode is for hosts with no
//!   such proxy.

#![warn(missing_docs)]

use std::{collections::HashMap, net::IpAddr, sync::OnceLock, time::Instant};

use parking_lot::Mutex;
use wafer_block::{
    match_path, meta::META_AUTH_USER_ROLES, Block, BlockConfig, BlockInfo, ConfigVar, Context,
    ErrorCode, InputStream, InputType, LifecycleEvent, LifecycleType, Message, OutputStream,
    WaferError,
};
use wafer_block_macro::wafer_async_trait;

/// Default routes for the stats / monitoring endpoints. Overridable via
/// the `stats_path` / `monitoring_path` flow_config keys (per request)
/// or via the block-level config JSON passed to `lifecycle(Init)`.
const DEFAULT_STATS_PATH: &str = "/_stats";
const DEFAULT_MONITORING_PATH: &str = "/_monitoring";
/// Default `stats_access`: role-gated.
const DEFAULT_STATS_ACCESS: &str = "roles";
/// Default `stats_roles`: the roles that may read the stats in `roles` mode.
const DEFAULT_STATS_ROLES: &str = "admin";

/// The route key a request is counted under when no registered block
/// declares an endpoint matching its path.
pub const UNMATCHED_ROUTE: &str = "(unmatched)";

/// Middleware block that tracks per-request metrics and serves the JSON
/// stats endpoint. Singleton per Wafer instance; auto-registered via
/// [`wafer_block::register_static_block!`].
pub(crate) struct MonitoringBlock {
    start_time: Instant,
    stats: Mutex<MonitoringStats>,
    /// Init-cached settings (write-once). `handle()` prefers per-request
    /// `ctx.config_get` and falls back to these, or to
    /// [`Settings::default`] when Init supplied no config.
    settings: OnceLock<Settings>,
}

/// Who may read the stats endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
enum StatsAccess {
    /// The caller's `auth.user_roles` must contain one of these roles.
    Roles(Vec<String>),
    /// The caller's `req.client.ip` must be a loopback address.
    Loopback,
}

impl StatsAccess {
    /// Parse `stats_access` (+ `stats_roles` for the `roles` mode). An
    /// unknown mode or an empty role list is an error, never a wider policy.
    fn parse(mode: &str, roles: &str) -> Result<Self, WaferError> {
        match mode {
            "roles" => {
                let roles: Vec<String> = roles
                    .split(',')
                    .map(str::trim)
                    .filter(|r| !r.is_empty())
                    .map(String::from)
                    .collect();
                if roles.is_empty() {
                    return Err(invalid("`stats_roles` must name at least one role"));
                }
                Ok(Self::Roles(roles))
            }
            "loopback" => Ok(Self::Loopback),
            other => Err(invalid(&format!(
                "`stats_access` must be `roles` or `loopback`, got {other:?}"
            ))),
        }
    }

    /// Whether `msg`'s caller may read the stats.
    fn admits(&self, msg: &Message) -> bool {
        match self {
            Self::Roles(allowed) => msg
                .get_meta(META_AUTH_USER_ROLES)
                .split(',')
                .map(str::trim)
                .any(|role| !role.is_empty() && allowed.iter().any(|a| a == role)),
            Self::Loopback => is_loopback_addr(msg.remote_addr()),
        }
    }
}

fn invalid(message: &str) -> WaferError {
    WaferError {
        code: ErrorCode::InvalidArgument,
        message: format!("wafer-run/monitoring: {message}"),
        meta: vec![],
    }
}

#[derive(Clone)]
struct Settings {
    stats_path: String,
    monitoring_path: String,
    stats_access: String,
    stats_roles: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            stats_path: DEFAULT_STATS_PATH.into(),
            monitoring_path: DEFAULT_MONITORING_PATH.into(),
            stats_access: DEFAULT_STATS_ACCESS.into(),
            stats_roles: DEFAULT_STATS_ROLES.into(),
        }
    }
}

struct MonitoringStats {
    total_requests: u64,
    /// Request count per route template (bounded by the declared endpoints,
    /// plus [`UNMATCHED_ROUTE`]).
    route_counts: HashMap<String, u64>,
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
                route_counts: HashMap::new(),
            }),
            settings: OnceLock::new(),
        }
    }

    /// Resolve the settings from the per-request context (preferred) or the
    /// Init-cached settings (or their defaults when Init supplied no config).
    fn resolved_settings(&self, ctx: &dyn Context) -> Settings {
        let cached = self.settings.get().cloned().unwrap_or_default();
        let get =
            |key: &str, fallback: String| ctx.config_get(key).map_or(fallback, ToString::to_string);
        Settings {
            stats_path: get("stats_path", cached.stats_path),
            monitoring_path: get("monitoring_path", cached.monitoring_path),
            stats_access: get("stats_access", cached.stats_access),
            stats_roles: get("stats_roles", cached.stats_roles),
        }
    }
}

/// The route template `path` is counted under: the first declared
/// endpoint path equal to it, else the first template matching it, else
/// [`UNMATCHED_ROUTE`]. Every key is therefore declared by some block, so
/// no request-supplied segment is ever stored.
fn route_template(blocks: &[BlockInfo], path: &str) -> String {
    let templates = || {
        blocks
            .iter()
            .flat_map(|b| b.endpoints.iter())
            .map(|ep| &ep.path)
    };
    templates()
        .find(|t| t.as_str() == path)
        .or_else(|| templates().find(|t| match_path(t, path)))
        .map_or_else(|| UNMATCHED_ROUTE.to_string(), Clone::clone)
}

/// Returns true when `addr` parses as a loopback IP — 127.0.0.0/8
/// (IPv4), `::1` (IPv6), or an IPv4-mapped IPv6 like `::ffff:127.0.0.1`.
///
/// Dual-stack listening sockets routinely deliver IPv4 loopback
/// connections as `::ffff:127.x.y.z`, so the mapped-IPv6 case must be
/// unwrapped before [`Ipv4Addr::is_loopback`] sees it — `IpAddr::is_loopback`
/// alone says `false` for those.
///
/// False for empty / unparseable strings: a caller that did not say where
/// it came from is not known to be local.
///
/// [`Ipv4Addr::is_loopback`]: std::net::Ipv4Addr::is_loopback
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
                 (uptime / per-route counts). Access per `stats_access`.",
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
            ConfigVar::new(
                "stats_access",
                "Who may read the stats: `roles` (the caller's \
                 auth.user_roles must hold one of `stats_roles`) or \
                 `loopback` (the caller's IP must be loopback; unsafe \
                 behind a same-host reverse proxy).",
                DEFAULT_STATS_ACCESS,
            )
            .name("Stats Access")
            .input_type(InputType::Select)
            .options(&[("roles", "Roles"), ("loopback", "Loopback")]),
            ConfigVar::new(
                "stats_roles",
                "Comma-separated roles that may read the stats when \
                 `stats_access` is `roles`.",
                DEFAULT_STATS_ROLES,
            )
            .name("Stats Roles"),
        ])
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let path = msg.path();
        let settings = self.resolved_settings(ctx);

        if path == settings.stats_path || path == settings.monitoring_path {
            let access = match StatsAccess::parse(&settings.stats_access, &settings.stats_roles) {
                Ok(access) => access,
                Err(e) => return OutputStream::error(e),
            };
            if !access.admits(&msg) {
                return OutputStream::error(WaferError {
                    code: ErrorCode::PermissionDenied,
                    message: "stats endpoint access denied".to_string(),
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
                    "routes": stats.route_counts,
                })
            };
            // `ok_json` sets `Content-Type: application/json` and turns a
            // serialization failure into a logged `Internal` error rather
            // than an empty 200; the sibling inspector block uses it too.
            return wafer_block::response::ok_json(&payload);
        }

        let route = route_template(ctx.registered_blocks(), path);
        {
            let mut stats = self.stats.lock();
            stats.total_requests += 1;
            *stats.route_counts.entry(route).or_insert(0) += 1;
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
            let settings = Settings {
                stats_path: config.str_or("stats_path", DEFAULT_STATS_PATH).to_string(),
                monitoring_path: config
                    .str_or("monitoring_path", DEFAULT_MONITORING_PATH)
                    .to_string(),
                stats_access: config
                    .str_or("stats_access", DEFAULT_STATS_ACCESS)
                    .to_string(),
                stats_roles: config
                    .str_or("stats_roles", DEFAULT_STATS_ROLES)
                    .to_string(),
            };
            StatsAccess::parse(&settings.stats_access, &settings.stats_roles)?;
            // Write-once: Init fires a single time per registration.
            let _ = self.settings.set(settings);
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

    mod endpoint {
        //! The stats endpoint and the counters, driven through a real
        //! runtime's `run_block`.

        use std::sync::Arc;

        use serde_json::json;
        use wafer_block::{
            streams::output::TerminalNotResponse, BlockEndpoint, InputStream, Message,
            OutputStream, META_REQ_ACTION, META_REQ_CLIENT_IP, META_REQ_RESOURCE,
        };
        use wafer_block_macro::wafer_async_trait;
        use wafer_test_support::builder::WaferBuilder;

        use super::super::*;

        /// Declares one templated endpoint, so a request under it has a
        /// route to be counted as.
        struct Reset;

        #[wafer_async_trait]
        impl Block for Reset {
            fn info(&self) -> BlockInfo {
                BlockInfo::new("test-org/reset", "0.0.1", "http-handler@v1", "test")
                    .endpoints(vec![BlockEndpoint::get("/b/reset/{token}")])
            }

            async fn handle(&self, _: &dyn Context, msg: Message, _: InputStream) -> OutputStream {
                OutputStream::continue_with(msg)
            }
        }

        async fn wafer(config: Option<serde_json::Value>) -> Arc<wafer_run::Wafer> {
            let mut b = WaferBuilder::new()
                .with_block("wafer-run/monitoring", Arc::new(MonitoringBlock::new()))
                .with_block("test-org/reset", Arc::new(Reset));
            if let Some(cfg) = config {
                b = b.with_config("wafer-run/monitoring", cfg);
            }
            b.build().await.expect("build")
        }

        async fn request(
            wafer: &wafer_run::Wafer,
            path: &str,
            meta: &[(&str, &str)],
        ) -> Result<wafer_block::streams::output::BufferedResponse, TerminalNotResponse> {
            let mut msg = Message::new(format!("GET:{path}"));
            msg.set_meta(META_REQ_ACTION, "retrieve");
            msg.set_meta(META_REQ_RESOURCE, path);
            for (k, v) in meta {
                msg.set_meta(*k, *v);
            }
            wafer
                .run_block("wafer-run/monitoring", msg, InputStream::empty())
                .await
                .collect_buffered()
                .await
        }

        fn denied(
            out: Result<wafer_block::streams::output::BufferedResponse, TerminalNotResponse>,
        ) -> bool {
            matches!(out, Err(TerminalNotResponse::Error(e)) if e.code == ErrorCode::PermissionDenied)
        }

        /// An internal or FFI caller that sets no `req.client.ip` was
        /// treated as local and served the stats.
        #[tokio::test]
        async fn stats_without_a_client_ip_is_denied() {
            for config in [None, Some(json!({ "stats_access": "loopback" }))] {
                let wafer = wafer(config.clone()).await;
                assert!(denied(request(&wafer, "/_stats", &[]).await), "{config:?}");
                assert!(
                    denied(request(&wafer, "/_monitoring", &[]).await),
                    "{config:?}"
                );
            }
        }

        /// By default a loopback peer — every request behind a same-host
        /// reverse proxy — is not enough; the caller needs a stats role.
        #[tokio::test]
        async fn default_access_needs_a_role_not_a_loopback_peer() {
            let wafer = wafer(None).await;
            assert!(denied(
                request(&wafer, "/_stats", &[(META_REQ_CLIENT_IP, "127.0.0.1")]).await
            ));
            assert!(denied(
                request(&wafer, "/_stats", &[("auth.user_roles", "viewer")]).await
            ));
            request(&wafer, "/_stats", &[("auth.user_roles", "viewer, admin")])
                .await
                .expect("admin reads the stats");
        }

        #[tokio::test]
        async fn loopback_mode_admits_a_loopback_peer_only() {
            let wafer = wafer(Some(json!({ "stats_access": "loopback" }))).await;
            request(
                &wafer,
                "/_stats",
                &[(META_REQ_CLIENT_IP, "::ffff:127.0.0.1")],
            )
            .await
            .expect("loopback reads the stats");
            assert!(denied(
                request(&wafer, "/_stats", &[(META_REQ_CLIENT_IP, "10.0.0.1")]).await
            ));
        }

        #[tokio::test]
        async fn unknown_access_mode_fails_init() {
            let wafer = wafer(Some(json!({ "stats_access": "anyone" }))).await;
            match request(&wafer, "/_stats", &[("auth.user_roles", "admin")]).await {
                Err(TerminalNotResponse::Error(e)) => {
                    assert!(e.message.contains("stats_access"), "{e:?}");
                }
                other => panic!("an unknown stats_access must fail Init, got {other:?}"),
            }
        }

        /// A reset token in the path and an undeclared share link must not
        /// be stored or served; both are counted under a declared template
        /// or the unmatched bucket.
        #[tokio::test]
        async fn stats_count_route_templates_not_raw_paths() {
            let wafer = wafer(None).await;
            for path in ["/b/reset/T0KEN-abc", "/b/reset/T0KEN-def", "/s/SHARE-xyz"] {
                assert!(matches!(
                    request(&wafer, path, &[]).await,
                    Err(TerminalNotResponse::Continue(_))
                ));
            }
            let resp = request(&wafer, "/_stats", &[("auth.user_roles", "admin")])
                .await
                .expect("admin reads the stats");
            let body: serde_json::Value = serde_json::from_slice(&resp.body).expect("JSON");
            assert_eq!(body["total_requests"], 3);
            assert_eq!(
                body["routes"],
                json!({ "/b/reset/{token}": 2, UNMATCHED_ROUTE: 1 })
            );
            let text = String::from_utf8(resp.body).expect("utf-8");
            assert!(!text.contains("T0KEN") && !text.contains("SHARE"), "{text}");
        }
    }
}
