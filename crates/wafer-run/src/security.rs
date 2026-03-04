/// SSRF defense-in-depth: block private/internal IPs and non-HTTP schemes.
///
/// This is the single shared implementation used by both the WASM host path
/// and the native context path.
///
/// Uses the `url` crate for proper parsing to prevent bypasses via userinfo
/// (e.g. `http://user@127.0.0.1/`), percent-encoding (e.g. `%31%32%37`),
/// or other URL tricks.
pub fn is_blocked_url(raw: &str) -> bool {
    let parsed = match url::Url::parse(raw) {
        Ok(u) => u,
        Err(_) => return true, // unparseable → block
    };

    // Only allow http and https schemes
    match parsed.scheme() {
        "http" | "https" => {}
        _ => return true,
    }

    match parsed.host() {
        None => true, // no host → block
        Some(url::Host::Domain(domain)) => {
            domain.eq_ignore_ascii_case("localhost")
        }
        Some(url::Host::Ipv4(ip)) => is_blocked_ipv4(ip),
        Some(url::Host::Ipv6(ip)) => is_blocked_ipv6(ip),
    }
}

fn is_blocked_ipv4(ip: std::net::Ipv4Addr) -> bool {
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
    false
}

fn is_blocked_ipv6(ip: std::net::Ipv6Addr) -> bool {
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
