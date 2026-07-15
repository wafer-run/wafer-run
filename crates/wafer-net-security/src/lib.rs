//! SSRF defenses shared by every native-side fetcher in the workspace.
//!
//! Sits below both `wafer-core` and `wafer-run` (which do not depend on each
//! other) so the same predicates and resolver guard every outbound HTTP
//! surface: the `wafer-run/network` service block (`wafer-block-network`) and
//! the runtime's registry/manifest downloads (`wafer-run`, SEC-09).
//! `wafer_core::security` re-exports the predicates for existing consumers.
//!
//! Two layers, applied together by callers:
//! - [`is_blocked_url`] — URL-level pre-check: scheme, `localhost`, and
//!   IP-literal hosts. Cheap, synchronous, catches by-name hits.
//! - [`SsrfFilteringResolver`] — DNS-resolution filter: drops resolved IPs
//!   that [`is_blocked_ip`] rejects, defending against DNS rebinding
//!   (SEC-019) where a public-looking hostname resolves to a private IP.
//!
//! The `allow-private-network` Cargo feature disables both layers for local
//! development and integration tests; it is a compile-time escape hatch by
//! design (SEC-018) so the bypass cannot be flipped on a live deploy.

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

    // ::ffff:0:0/96 (IPv4-mapped IPv6) — check the embedded IPv4 address
    if segments[0..5] == [0, 0, 0, 0, 0] && segments[5] == 0xffff {
        let ipv4 = std::net::Ipv4Addr::new(
            (segments[6] >> 8) as u8,
            (segments[6] & 0xff) as u8,
            (segments[7] >> 8) as u8,
            (segments[7] & 0xff) as u8,
        );
        return is_blocked_ipv4(ipv4);
    }

    false
}

#[cfg(not(target_arch = "wasm32"))]
mod resolver {
    use std::net::SocketAddr;

    use reqwest::dns::{Addrs, Name, Resolve, Resolving};

    /// `reqwest::dns::Resolve` impl that performs the system DNS lookup and
    /// then drops any resolved socket whose IP would be blocked by
    /// [`is_blocked_ip`](super::is_blocked_ip). Defends against DNS rebinding
    /// (SEC-019): a public-looking hostname that resolves to `127.0.0.1` (or
    /// any private/loopback/link-local IP) is rejected here, before the TCP
    /// connection is established.
    ///
    /// When the `allow-private-network` Cargo feature is enabled, the filter
    /// is disabled (resolved IPs are passed through unchanged). The feature is
    /// off by default and intended only for local development / integration
    /// tests.
    pub struct SsrfFilteringResolver;

    impl Resolve for SsrfFilteringResolver {
        // The intermediate `Vec` collect is intentional: we need the resolved
        // addresses materialised so the cfg-gated filter below (which is
        // compiled out under `allow-private-network`) can inspect them, and so
        // reqwest gets an owned `Box<dyn Iterator + Send>` rather than the
        // borrowing iterator returned by `tokio::net::lookup_host`.
        #[expect(
            clippy::needless_collect,
            reason = "Vec is reused: SSRF filter inspects + reqwest takes owned Iterator+Send"
        )]
        fn resolve(&self, name: Name) -> Resolving {
            let host = name.as_str().to_string();
            Box::pin(async move {
                // Port `0` here — reqwest replaces it with the URL-derived port
                // (see `reqwest::dns::resolve::DynResolver::http_resolve`).
                //
                // Collect into a `Vec` so we can both inspect the resolved
                // addresses (for the SSRF filter below) and hand reqwest an
                // owned `Iterator + Send` (the trait object cannot be backed
                // by the borrowing iterator `lookup_host` returns).
                let resolved: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                    .await
                    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
                    .collect();

                #[cfg(feature = "allow-private-network")]
                {
                    let iter: Addrs = Box::new(resolved.into_iter());
                    return Ok(iter);
                }

                #[cfg(not(feature = "allow-private-network"))]
                {
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

                    let iter: Addrs = Box::new(filtered.into_iter());
                    Ok(iter)
                }
            })
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use resolver::SsrfFilteringResolver;

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
        // Just below the /4 must stay allowed.
        assert!(!is_blocked_url("http://239.255.255.255"));
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
