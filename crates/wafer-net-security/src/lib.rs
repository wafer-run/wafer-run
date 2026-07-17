//! SSRF defenses shared by every native-side fetcher in the workspace.
//!
//! Sits below both `wafer-core` and `wafer-run` (which do not depend on each
//! other) so the same predicates and resolver guard every outbound HTTP
//! surface: the `wafer-run/network` service block (`wafer-block-network`) and
//! the runtime's registry/manifest downloads (`wafer-run`, SEC-09).
//! `wafer_core::security` re-exports the predicates for existing consumers.
//!
//! Three layers, applied together by callers:
//! - [`is_blocked_url`] — URL-level pre-check: scheme, `localhost`, and
//!   IP-literal hosts. Cheap, synchronous, catches by-name hits.
//! - [`SsrfFilteringResolver`] — DNS-resolution filter: drops resolved IPs
//!   that [`is_blocked_ip`] rejects, defending against DNS rebinding
//!   (SEC-019) where a public-looking hostname resolves to a private IP.
//!   Because reqwest connects to exactly the addresses this resolver returns
//!   (no second lookup), the IP that is validated is the IP that is dialed —
//!   there is no resolve-then-reconnect TOCTOU window.
//! - [`ssrf_redirect_policy`] — redirect filter: revalidates every 3xx hop's
//!   target URL (and, via the resolver above, its resolved IP) so a public
//!   first hop cannot bounce the request to an internal address. Bounded hop
//!   count. Unlike a blanket `redirect::Policy::none()`, legitimate public
//!   redirects are still followed.
//!
//! The `allow-private-network` Cargo feature disables the enforcement in all
//! three layers for local development and integration tests; it is a
//! compile-time escape hatch by design (SEC-018) so the bypass cannot be
//! flipped on a live deploy.

/// SSRF defense-in-depth: block private/internal IPs and non-HTTP schemes.
///
/// This is the single shared implementation used by both the WASM host path
/// and the native context path.
///
/// Uses the `url` crate for proper parsing to prevent bypasses via userinfo
/// (e.g. `http://user@127.0.0.1/`), percent-encoding (e.g. `%31%32%37`),
/// or other URL tricks.
pub fn is_blocked_url(raw: &str) -> bool {
    let Ok(parsed) = url::Url::parse(raw) else {
        return true; // unparseable → block
    };

    // Only allow http and https schemes
    match parsed.scheme() {
        "http" | "https" => {}
        _ => return true,
    }

    match parsed.host() {
        None => true, // no host → block
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => is_blocked_ipv4(ip),
        Some(url::Host::Ipv6(ip)) => is_blocked_ipv6(ip),
    }
}

/// Check whether a resolved IP address is private/loopback/link-local and
/// should be blocked by SSRF defense. Defends against DNS rebinding by being
/// callable on the IPs returned by a DNS resolver, not just URL hosts.
/// See SEC-019.
pub fn is_blocked_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => is_blocked_ipv4(v4),
        std::net::IpAddr::V6(v6) => is_blocked_ipv6(v6),
    }
}

/// Check whether an IPv4 address is private/loopback/link-local/etc and
/// should be blocked by SSRF defense. Exposed so DNS resolvers can validate
/// resolved IPs (defends against DNS rebinding — see SEC-019).
pub fn is_blocked_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let o = ip.octets();
    // 0.0.0.0/8 (current host)
    if o[0] == 0 {
        return true;
    }
    // 127.0.0.0/8 (loopback)
    if o[0] == 127 {
        return true;
    }
    // 10.0.0.0/8 (private)
    if o[0] == 10 {
        return true;
    }
    // 172.16.0.0/12 (private)
    if o[0] == 172 && (16..=31).contains(&o[1]) {
        return true;
    }
    // 192.168.0.0/16 (private)
    if o[0] == 192 && o[1] == 168 {
        return true;
    }
    // 169.254.0.0/16 (link-local)
    if o[0] == 169 && o[1] == 254 {
        return true;
    }
    // 100.64.0.0/10 (carrier-grade NAT — routable internal infra on cloud
    // providers). The std `Ipv4Addr::is_shared()` predicate is still unstable
    // (feature `ip`, issue #27709) on the stable toolchain this crate targets,
    // so the /10 mask is applied by hand: the first octet is 100 and the high
    // two bits of the second octet are `01` (i.e. 64..=127).
    if o[0] == 100 && (64..=127).contains(&o[1]) {
        return true;
    }
    // 192.0.0.0/24 (IETF Protocol Assignments). Not covered by any stable std
    // predicate (`is_documentation()` only matches the TEST-NET ranges and is
    // itself unstable), so it is matched explicitly.
    if o[0] == 192 && o[1] == 0 && o[2] == 0 {
        return true;
    }
    // 198.18.0.0/15 (benchmarking). `Ipv4Addr::is_benchmarking()` is unstable
    // (feature `ip`), so the /15 is matched by hand: first octet 198 and the
    // low bit of the second octet cleared selects 18 and 19.
    if o[0] == 198 && (o[1] == 18 || o[1] == 19) {
        return true;
    }
    // 224.0.0.0/4 (multicast). Not globally-routable unicast, so an SSRF guard
    // must reject it — e.g. 224.0.0.1 (all-hosts), 224.0.0.251 (mDNS),
    // 239.255.255.250 (SSDP/UPnP), any of which reach services on the local
    // segment. `Ipv4Addr::is_multicast()` is stable, so use it directly.
    if ip.is_multicast() {
        return true;
    }
    // 240.0.0.0/4 (reserved, incl. the former Class E space).
    // `Ipv4Addr::is_reserved()` is unstable (feature `ip`); the /4 is the high
    // nibble of the first octet being `1111` (i.e. >= 240). Note std's
    // `is_reserved()` deliberately excludes 255.255.255.255 (broadcast); the
    // broadcast address is covered separately below, so blocking the whole /4
    // here is strictly safe for an SSRF guard.
    if o[0] >= 240 {
        return true;
    }
    // 255.255.255.255 (limited broadcast). `Ipv4Addr::is_broadcast()` is the
    // one relevant predicate that is stable, so use it directly. (This is
    // already covered by the 240.0.0.0/4 arm above, but the explicit check
    // documents intent and stays correct if that arm is ever narrowed.)
    if ip.is_broadcast() {
        return true;
    }
    false
}

/// Decode the IPv4 address embedded in two consecutive IPv6 segments
/// (`hi:lo`, big-endian octets). Shared by every IPv6 arm of
/// [`is_blocked_ipv6`] that wraps a v4 address (IPv4-mapped, NAT64,
/// IPv4-compatible, 6to4).
fn embedded_v4(hi: u16, lo: u16) -> std::net::Ipv4Addr {
    std::net::Ipv4Addr::new(
        (hi >> 8) as u8,
        (hi & 0xff) as u8,
        (lo >> 8) as u8,
        (lo & 0xff) as u8,
    )
}

/// Check whether an IPv6 address is loopback/private/link-local/etc and
/// should be blocked by SSRF defense. Exposed so DNS resolvers can validate
/// resolved IPs (defends against DNS rebinding — see SEC-019).
pub fn is_blocked_ipv6(ip: std::net::Ipv6Addr) -> bool {
    let segments = ip.segments();

    // ::1 (loopback)
    if ip == std::net::Ipv6Addr::LOCALHOST {
        return true;
    }

    // :: (unspecified)
    if ip == std::net::Ipv6Addr::UNSPECIFIED {
        return true;
    }

    // fe80::/10 (link-local)
    if segments[0] & 0xffc0 == 0xfe80 {
        return true;
    }

    // fc00::/7 (unique local / private)
    if segments[0] & 0xfe00 == 0xfc00 {
        return true;
    }

    // ff00::/8 (multicast). Mirrors the IPv4 224.0.0.0/4 arm — not globally
    // routable unicast, so an SSRF guard rejects it (e.g. ff02::1 all-nodes on
    // the local link). `Ipv6Addr::is_multicast()` is stable; IPv4-mapped
    // addresses (`::ffff:0:0/96`, first segment 0) are never multicast here and
    // are handled by the dedicated arm below.
    if ip.is_multicast() {
        return true;
    }

    // ::ffff:0:0/96 (IPv4-mapped IPv6) — validate the embedded IPv4 address.
    if segments[0..5] == [0, 0, 0, 0, 0] && segments[5] == 0xffff {
        return is_blocked_ipv4(embedded_v4(segments[6], segments[7]));
    }

    // 64:ff9b::/96 (NAT64 well-known prefix, RFC 6052). In a DNS64 environment
    // a stub resolver synthesizes AAAA records of this form for A-only hosts,
    // so a name pointing at e.g. 169.254.169.254 resolves to
    // `64:ff9b::a9fe:a9fe` (which passes an unaware filter) and NAT64 routes it
    // back to the private v4. Decode the embedded v4 (low 32 bits) and validate
    // it, so a NAT64 route to a genuinely PUBLIC v4 stays allowed rather than
    // blocking the entire /96.
    if segments[0] == 0x0064 && segments[1] == 0xff9b && segments[2..6] == [0, 0, 0, 0] {
        return is_blocked_ipv4(embedded_v4(segments[6], segments[7]));
    }

    // ::/96 IPv4-compatible IPv6 (deprecated, RFC 4291 §2.5.5.1): `::a.b.c.d`.
    // The all-zero (`::`) and `::1` cases returned above; any other embedding
    // (e.g. `::127.0.0.1` → `::7f00:1`) is validated as its embedded v4 so a
    // loopback/private target in this legacy form is still blocked.
    if segments[0..6] == [0, 0, 0, 0, 0, 0] {
        return is_blocked_ipv4(embedded_v4(segments[6], segments[7]));
    }

    // 2002::/16 6to4 (RFC 3056): `2002:WWXX:YYZZ::/48` embeds the IPv4 address
    // W.X.Y.Z in segments 1–2. Validate it so a 6to4 address wrapping a
    // private/loopback v4 (e.g. `2002:7f00:1::` = 127.0.0.1, or
    // `2002:a9fe:a9fe::` = 169.254.169.254) is blocked; a 6to4 address over a
    // public v4 stays allowed.
    if segments[0] == 0x2002 {
        return is_blocked_ipv4(embedded_v4(segments[1], segments[2]));
    }

    false
}

/// Maximum number of redirect hops [`ssrf_redirect_policy`] follows before it
/// aborts the chain. Bounds redirect-loop / amplification; matches reqwest's
/// historical default of 10.
#[cfg(not(target_arch = "wasm32"))]
pub const MAX_REDIRECT_HOPS: usize = 10;

#[cfg(not(target_arch = "wasm32"))]
mod resolver {
    use std::net::SocketAddr;

    use reqwest::dns::{Addrs, Name, Resolve, Resolving};

    /// Drop resolved socket addresses whose IP would be blocked by
    /// [`is_blocked_ip`](super::is_blocked_ip). Factored out of
    /// [`SsrfFilteringResolver::resolve`] so the DNS-rebinding filter is unit
    /// testable with synthetic address lists (no live DNS). Returns an error —
    /// which reqwest surfaces as a resolution failure — when every resolved IP
    /// is private/loopback/link-local, so a public-looking host that resolves
    /// entirely to internal IPs is rejected before any TCP connection.
    ///
    /// Compiled out under `allow-private-network`, where the resolver passes
    /// results through unfiltered.
    #[cfg(not(feature = "allow-private-network"))]
    fn filter_resolved(
        host: &str,
        resolved: Vec<SocketAddr>,
    ) -> Result<Vec<SocketAddr>, Box<dyn std::error::Error + Send + Sync>> {
        let filtered: Vec<SocketAddr> = resolved
            .into_iter()
            .filter(|s| !super::is_blocked_ip(s.ip()))
            .collect();

        if filtered.is_empty() {
            return Err(format!(
                "DNS resolution for {host} returned no public IPs (blocked: private/loopback/link-local)"
            )
            .into());
        }
        Ok(filtered)
    }

    /// `reqwest::dns::Resolve` impl that performs the system DNS lookup and
    /// then drops any resolved socket whose IP would be blocked by
    /// [`is_blocked_ip`](super::is_blocked_ip). Defends against DNS rebinding
    /// (SEC-019): a public-looking hostname that resolves to `127.0.0.1` (or
    /// any private/loopback/link-local IP) is rejected here, before the TCP
    /// connection is established. reqwest dials exactly the addresses returned
    /// here, so the validated IP is the connected IP (no re-resolve TOCTOU).
    ///
    /// When the `allow-private-network` Cargo feature is enabled, the filter
    /// is disabled (resolved IPs are passed through unchanged). The feature is
    /// off by default and intended only for local development / integration
    /// tests.
    pub struct SsrfFilteringResolver;

    impl Resolve for SsrfFilteringResolver {
        // The `Vec` collect is intentional and not a `needless_collect`: the
        // borrowing iterator `tokio::net::lookup_host` returns cannot back the
        // owned `Box<dyn Iterator + Send>` reqwest requires, and the filter
        // below needs the addresses materialised. clippy only flags it under
        // `allow-private-network` (where the filter is compiled out), so the
        // suppression is an `allow`, not a feature-conditional `expect`.
        #[allow(clippy::needless_collect)]
        fn resolve(&self, name: Name) -> Resolving {
            let host = name.as_str().to_string();
            Box::pin(async move {
                // Port `0` here — reqwest replaces it with the URL-derived port
                // (see `reqwest::dns::resolve::DynResolver::http_resolve`).
                let resolved: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                    .await
                    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
                    .collect();

                // DNS-rebinding filter. Compiled out under
                // `allow-private-network`, where resolved IPs pass through
                // unchanged (local dev / integration tests).
                #[cfg(not(feature = "allow-private-network"))]
                let resolved = filter_resolved(&host, resolved)?;

                let iter: Addrs = Box::new(resolved.into_iter());
                Ok(iter)
            })
        }
    }

    /// Per-hop decision for the redirect policy, factored out of the reqwest
    /// `redirect::Policy` closure so the SSRF revalidation is unit testable
    /// without constructing reqwest's non-`pub` `Attempt`.
    ///
    /// Only compiled into the enforcing (default) build; under
    /// `allow-private-network` the policy is a plain bounded follow with no
    /// per-hop URL block, so this decision type is not needed there.
    #[cfg(not(feature = "allow-private-network"))]
    #[derive(Debug, PartialEq, Eq)]
    enum RedirectDecision {
        /// Safe to follow this hop.
        Follow,
        /// Abort: the redirect chain exceeded [`MAX_REDIRECT_HOPS`].
        TooManyHops,
        /// Abort: the hop target is a private/internal/non-http URL.
        Blocked,
    }

    /// Decide whether a single redirect hop to `next_url` (with `prior_hops`
    /// URLs already visited) may be followed. The URL-layer block mirrors
    /// [`is_blocked_url`](super::is_blocked_url); the hop's *resolved* IP is
    /// validated separately by [`SsrfFilteringResolver`] on connect, so a
    /// public-looking redirect target that rebinds to a private IP is still
    /// caught.
    #[cfg(not(feature = "allow-private-network"))]
    fn redirect_decision(next_url: &str, prior_hops: usize) -> RedirectDecision {
        if prior_hops >= super::MAX_REDIRECT_HOPS {
            return RedirectDecision::TooManyHops;
        }
        if super::is_blocked_url(next_url) {
            return RedirectDecision::Blocked;
        }
        RedirectDecision::Follow
    }

    /// Error surfaced to reqwest when a redirect hop is rejected, so the caller
    /// sees a descriptive SSRF message rather than a generic redirect failure.
    #[cfg(not(feature = "allow-private-network"))]
    #[derive(Debug)]
    struct RedirectBlocked(String);

    #[cfg(not(feature = "allow-private-network"))]
    impl std::fmt::Display for RedirectBlocked {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }

    #[cfg(not(feature = "allow-private-network"))]
    impl std::error::Error for RedirectBlocked {}

    /// reqwest redirect policy that revalidates every hop against the SSRF URL
    /// predicate ([`is_blocked_url`](super::is_blocked_url)) and bounds the hop
    /// count at [`MAX_REDIRECT_HOPS`]. Combined with [`SsrfFilteringResolver`]
    /// on the same client — which validates each hop's *resolved* IP — this
    /// closes the redirect-to-private vector (a public first hop cannot bounce
    /// the request to an internal address) while still following legitimate
    /// public redirects, unlike a blanket `redirect::Policy::none()`.
    #[cfg(not(feature = "allow-private-network"))]
    pub fn ssrf_redirect_policy() -> reqwest::redirect::Policy {
        reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url().as_str().to_string();
            match redirect_decision(&url, attempt.previous().len()) {
                RedirectDecision::Follow => attempt.follow(),
                RedirectDecision::TooManyHops => attempt.error(RedirectBlocked(format!(
                    "too many redirects (limit {})",
                    super::MAX_REDIRECT_HOPS
                ))),
                RedirectDecision::Blocked => attempt.error(RedirectBlocked(format!(
                    "redirect to private/internal address blocked: {url}"
                ))),
            }
        })
    }

    /// `allow-private-network` escape hatch: a plain bounded redirect follow
    /// (no per-hop URL block), mirroring the resolver passthrough. Intended
    /// only for local development / integration tests.
    #[cfg(feature = "allow-private-network")]
    pub fn ssrf_redirect_policy() -> reqwest::redirect::Policy {
        reqwest::redirect::Policy::limited(super::MAX_REDIRECT_HOPS)
    }

    // All items under test (`filter_resolved`, `redirect_decision`,
    // `RedirectDecision`) are compiled only in the enforcing build, so the
    // whole module is gated off under `allow-private-network`.
    #[cfg(all(test, not(feature = "allow-private-network")))]
    mod tests {
        use std::net::SocketAddr;

        use super::{filter_resolved, redirect_decision, RedirectDecision};

        /// The DNS-rebinding filter: a "public" host that resolves to a
        /// loopback socket is rejected (no public IP survives), while a public
        /// socket is kept. Exercises the same code path a real rebinding
        /// attack hits, deterministically and without live DNS.
        #[test]
        fn filter_resolved_rejects_all_private_keeps_public() {
            let loopback: SocketAddr = "127.0.0.1:0".parse().unwrap();
            let public: SocketAddr = "93.184.216.34:0".parse().unwrap();

            // A public-looking host that resolves ONLY to loopback → rejected.
            assert!(filter_resolved("rebind.example", vec![loopback]).is_err());

            // Resolves to a public IP → kept.
            let kept = filter_resolved("public.example", vec![public]).expect("public kept");
            assert_eq!(kept, vec![public]);

            // Mixed → only the public address survives (a hostile record cannot
            // smuggle a private IP alongside a public one).
            let mixed = filter_resolved("mixed.example", vec![loopback, public])
                .expect("at least one public");
            assert_eq!(mixed, vec![public]);
        }

        #[test]
        fn redirect_decision_bounds_hop_count() {
            assert_eq!(
                redirect_decision("https://example.com/", super::super::MAX_REDIRECT_HOPS),
                RedirectDecision::TooManyHops
            );
        }

        /// A redirect hop to a private/internal or non-http target is blocked;
        /// a public target is followed.
        #[test]
        fn redirect_decision_blocks_private_follows_public() {
            assert_eq!(
                redirect_decision("http://10.0.0.1/", 0),
                RedirectDecision::Blocked
            );
            assert_eq!(
                redirect_decision("http://169.254.169.254/latest/meta-data/", 1),
                RedirectDecision::Blocked
            );
            assert_eq!(
                redirect_decision("http://localhost/admin", 0),
                RedirectDecision::Blocked
            );
            assert_eq!(
                redirect_decision("file:///etc/passwd", 0),
                RedirectDecision::Blocked
            );
            assert_eq!(
                redirect_decision("https://example.com/next", 3),
                RedirectDecision::Follow
            );
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use resolver::{ssrf_redirect_policy, SsrfFilteringResolver};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blocks_non_http() {
        assert!(is_blocked_url("ftp://example.com"));
        assert!(is_blocked_url("file:///etc/passwd"));
        assert!(is_blocked_url("gopher://localhost"));
    }

    #[test]
    fn test_allows_public_http() {
        assert!(!is_blocked_url("https://example.com/api"));
        assert!(!is_blocked_url("http://93.184.216.34/path"));
    }

    #[test]
    fn test_blocks_localhost() {
        assert!(is_blocked_url("http://localhost/admin"));
        assert!(is_blocked_url("http://localhost:8080/admin"));
    }

    #[test]
    fn test_blocks_private_ipv4() {
        assert!(is_blocked_url("http://127.0.0.1"));
        assert!(is_blocked_url("http://10.0.0.1/api"));
        assert!(is_blocked_url("http://172.16.0.1"));
        assert!(is_blocked_url("http://192.168.1.1"));
        assert!(is_blocked_url("http://0.0.0.0"));
    }

    #[test]
    fn test_blocks_link_local_ipv4() {
        assert!(is_blocked_url("http://169.254.1.1"));
        assert!(is_blocked_url("http://169.254.169.254"));
    }

    #[test]
    fn test_blocks_carrier_grade_nat_ipv4() {
        // 100.64.0.0/10 — routable internal infra on cloud providers.
        assert!(is_blocked_url("http://100.64.0.1"));
        assert!(is_blocked_url("http://100.127.255.255"));
        // Just outside the /10 on both ends must stay allowed.
        assert!(!is_blocked_url("http://100.63.255.255"));
        assert!(!is_blocked_url("http://100.128.0.1"));
    }

    #[test]
    fn test_blocks_ietf_protocol_assignments_ipv4() {
        // 192.0.0.0/24 — IETF Protocol Assignments.
        assert!(is_blocked_url("http://192.0.0.1"));
        assert!(is_blocked_url("http://192.0.0.255"));
        // 192.0.2.0/24 (TEST-NET-1) is a different block; adjacent 192.0.1.x
        // is public — neither should be caught by the /24 above.
        assert!(!is_blocked_url("http://192.0.1.1"));
    }

    #[test]
    fn test_blocks_benchmarking_ipv4() {
        // 198.18.0.0/15 — network benchmarking.
        assert!(is_blocked_url("http://198.18.0.1"));
        assert!(is_blocked_url("http://198.19.255.255"));
        // Just outside the /15 must stay allowed.
        assert!(!is_blocked_url("http://198.17.255.255"));
        assert!(!is_blocked_url("http://198.20.0.1"));
    }

    #[test]
    fn test_blocks_reserved_ipv4() {
        // 240.0.0.0/4 — reserved (former Class E).
        assert!(is_blocked_url("http://240.0.0.1"));
        assert!(is_blocked_url("http://250.1.2.3"));
        // Just below the reserved+multicast span (224.0.0.0 – 255.255.255.255)
        // must stay allowed. 223.255.255.255 is public unicast.
        assert!(!is_blocked_url("http://223.255.255.255"));
    }

    #[test]
    fn test_blocks_multicast_ipv4() {
        // 224.0.0.0/4 — multicast (224.0.0.0 – 239.255.255.255).
        assert!(is_blocked_url("http://224.0.0.1")); // all-hosts
        assert!(is_blocked_url("http://224.0.0.251")); // mDNS
        assert!(is_blocked_url("http://239.255.255.250")); // SSDP/UPnP
        assert!(is_blocked_url("http://239.255.255.255"));
        // Boundaries: just outside the /4 on both ends stays allowed.
        assert!(!is_blocked_url("http://223.255.255.255"));
    }

    #[test]
    fn test_blocks_multicast_ipv6() {
        // ff00::/8 — multicast.
        assert!(is_blocked_url("http://[ff02::1]")); // all-nodes, link-local
        assert!(is_blocked_url("http://[ff00::1]"));
        assert!(is_blocked_url("http://[ff05::c]")); // site-local all-DHCP
    }

    #[test]
    fn test_blocks_broadcast_ipv4() {
        // 255.255.255.255 — limited broadcast.
        assert!(is_blocked_url("http://255.255.255.255"));
    }

    #[test]
    fn test_blocks_ipv6_loopback() {
        assert!(is_blocked_url("http://[::1]/admin"));
        assert!(is_blocked_url("http://[::1]:8080/admin"));
    }

    #[test]
    fn test_blocks_ipv6_private() {
        assert!(is_blocked_url("http://[fc00::1]"));
        assert!(is_blocked_url("http://[fd12:3456::1]"));
    }

    #[test]
    fn test_blocks_ipv6_link_local() {
        assert!(is_blocked_url("http://[fe80::1]"));
    }

    #[test]
    fn test_blocks_ipv4_mapped_ipv6() {
        assert!(is_blocked_url("http://[::ffff:127.0.0.1]"));
        assert!(is_blocked_url("http://[::ffff:10.0.0.1]"));
        assert!(!is_blocked_url("http://[::ffff:93.184.216.34]"));
    }

    #[test]
    fn test_blocks_nat64_dns64_embedded_private_v4() {
        use std::net::Ipv6Addr;
        // 64:ff9b::169.254.169.254 — DNS64 synthesis pointing NAT64 at the
        // cloud metadata service. 0xa9fe:0xa9fe == 169.254.169.254.
        assert!(is_blocked_ipv6(Ipv6Addr::new(
            0x64, 0xff9b, 0, 0, 0, 0, 0xa9fe, 0xa9fe
        )));
        assert!(is_blocked_url("http://[64:ff9b::a9fe:a9fe]"));
        // 64:ff9b::127.0.0.1 (0x7f00:0x0001).
        assert!(is_blocked_ipv6(Ipv6Addr::new(
            0x64, 0xff9b, 0, 0, 0, 0, 0x7f00, 0x0001
        )));
        // A NAT64 route to a genuinely PUBLIC v4 stays allowed — decode-and-
        // check, not a blanket /96 block. 0x5db8:0xd822 == 93.184.216.34.
        assert!(!is_blocked_ipv6(Ipv6Addr::new(
            0x64, 0xff9b, 0, 0, 0, 0, 0x5db8, 0xd822
        )));
        assert!(!is_blocked_url("http://[64:ff9b::5db8:d822]"));
    }

    #[test]
    fn test_blocks_ipv4_compatible_ipv6() {
        use std::net::Ipv6Addr;
        // ::127.0.0.1 (deprecated IPv4-compatible loopback) == ::7f00:1.
        assert!(is_blocked_ipv6(Ipv6Addr::new(
            0, 0, 0, 0, 0, 0, 0x7f00, 0x0001
        )));
        assert!(is_blocked_url("http://[::7f00:1]"));
        // ::10.0.0.1
        assert!(is_blocked_ipv6(Ipv6Addr::new(
            0, 0, 0, 0, 0, 0, 0x0a00, 0x0001
        )));
        // A public embedded v4 stays allowed (93.184.216.34).
        assert!(!is_blocked_ipv6(Ipv6Addr::new(
            0, 0, 0, 0, 0, 0, 0x5db8, 0xd822
        )));
    }

    #[test]
    fn test_blocks_6to4_embedded_private_v4() {
        use std::net::Ipv6Addr;
        // 2002:7f00:1:: embeds 127.0.0.1.
        assert!(is_blocked_ipv6(Ipv6Addr::new(
            0x2002, 0x7f00, 0x0001, 0, 0, 0, 0, 0
        )));
        assert!(is_blocked_url("http://[2002:7f00:1::]"));
        // 2002:a9fe:a9fe:: embeds 169.254.169.254 (metadata service).
        assert!(is_blocked_ipv6(Ipv6Addr::new(
            0x2002, 0xa9fe, 0xa9fe, 0, 0, 0, 0, 0
        )));
        // 6to4 wrapping a public v4 (93.184.216.34) stays allowed.
        assert!(!is_blocked_ipv6(Ipv6Addr::new(
            0x2002, 0x5db8, 0xd822, 0, 0, 0, 0, 0
        )));
    }

    #[test]
    fn test_blocks_userinfo_bypass() {
        // user@host should still check the actual host
        assert!(is_blocked_url("http://evil@127.0.0.1/"));
        assert!(is_blocked_url("http://user:pass@localhost/"));
        assert!(!is_blocked_url("http://user@example.com/"));
    }

    #[test]
    fn test_blocks_percent_encoded_ip() {
        // %31%32%37.0.0.1 == 127.0.0.1 when decoded — url crate handles this
        assert!(is_blocked_url("http://%31%32%37.0.0.1/"));
    }

    #[test]
    fn test_blocks_unparseable() {
        assert!(is_blocked_url("not-a-url"));
        assert!(is_blocked_url("://missing-scheme"));
    }

    /// Test [`SsrfFilteringResolver`] in isolation: a hostname that resolves
    /// to a loopback IP must produce an error. Uses `localhost` because the
    /// OS resolver returns `127.0.0.1` / `::1` for it reliably.
    ///
    /// (When built with `--features allow-private-network` the resolver is
    /// a passthrough, so this test only enforces the rejection in the
    /// default build.)
    #[cfg(all(not(target_arch = "wasm32"), not(feature = "allow-private-network")))]
    #[tokio::test]
    async fn dns_resolver_rejects_loopback_resolution() {
        use reqwest::dns::{Name, Resolve};

        let resolver = SsrfFilteringResolver;
        let name: Name = "localhost".parse().expect("parse name");
        let result = resolver.resolve(name).await;
        assert!(
            result.is_err(),
            "resolver must reject hostnames that resolve to loopback IPs"
        );
    }
}
