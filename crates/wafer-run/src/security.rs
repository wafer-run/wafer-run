/// SSRF defense-in-depth: block private/internal IPs and non-HTTP schemes.
///
/// This is the single shared implementation used by both the WASM host path
/// and the native context path.
pub fn is_blocked_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return true;
    }
    let after_scheme = if lower.starts_with("https://") {
        &url[8..]
    } else {
        &url[7..]
    };
    let host = after_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");

    if host == "localhost" {
        return true;
    }

    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        let o = ip.octets();
        // 127.0.0.0/8
        if o[0] == 127 {
            return true;
        }
        // 10.0.0.0/8
        if o[0] == 10 {
            return true;
        }
        // 172.16.0.0/12
        if o[0] == 172 && (16..=31).contains(&o[1]) {
            return true;
        }
        // 192.168.0.0/16
        if o[0] == 192 && o[1] == 168 {
            return true;
        }
        // 169.254.169.254 (cloud metadata endpoint)
        if o[0] == 169 && o[1] == 254 && o[2] == 169 && o[3] == 254 {
            return true;
        }
    }

    false
}
