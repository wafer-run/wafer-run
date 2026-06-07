//! SSRF defenses shared by host- and native-side fetchers.
//!
//! Lives in `wafer-core` (not `wafer-run`) so that leaf blocks such as
//! `wafer-block-network` can apply the same SSRF predicates without taking a
//! dependency on the runtime crate. `wafer-block-network` is currently the sole
//! consumer; `wafer-run` no longer references these predicates (the module was
//! moved here, not re-exported).

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
}
