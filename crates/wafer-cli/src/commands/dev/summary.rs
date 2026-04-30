//! Boot-event tracing parser.
//!
//! `wafer dev` tees the child's stderr lines through `parse_event`. Lines
//! that contain `target = "wafer.runtime"` events are extracted and used to
//! build the boot banner. Other lines are ignored.

use once_cell::sync::Lazy;
use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootEvent {
    Starting { blocks: usize },
    FlowRegistered { flow: String },
    Listening { addr: String },
}

// Regexes for extracting structured tracing fields. tracing's default
// formatter prints fields after the message, e.g.:
//   ... INFO wafer-run::runtime::lifecycle: wafer runtime starting blocks=12 event="starting"
//
// We don't pin the prefix; we look for `event="..."` and the relevant fields
// anywhere on the line. The field formatter is stable across tracing
// versions.
static EVENT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"event="([^"]+)""#).unwrap());
static BLOCKS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"\bblocks=(\d+)\b"#).unwrap());
static FLOW_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"\bflow=(\S+)"#).unwrap());
static ADDR_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"\baddr=(\S+)"#).unwrap());

/// Try to extract a BootEvent from a single stderr line. Returns `None` for
/// any line that isn't a wafer.runtime event.
pub fn parse_event(line: &str) -> Option<BootEvent> {
    // Discriminator: every wafer.runtime event has `event="..."`. Non-wafer
    // log lines won't have this exact string from the wafer codebase.
    let event = EVENT_RE.captures(line)?.get(1)?.as_str();
    match event {
        "starting" => {
            let blocks = BLOCKS_RE.captures(line)?.get(1)?.as_str().parse().ok()?;
            Some(BootEvent::Starting { blocks })
        }
        "flow_registered" => {
            let flow = FLOW_RE.captures(line)?.get(1)?.as_str().to_string();
            Some(BootEvent::FlowRegistered { flow })
        }
        "listening" => {
            let addr = ADDR_RE.captures(line)?.get(1)?.as_str().to_string();
            Some(BootEvent::Listening { addr })
        }
        _ => None,
    }
}

/// Pretty-format the boot banner. Missing fields are omitted gracefully.
pub fn format_banner(blocks: Option<usize>, flows: usize, addr: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(addr) = addr {
        parts.push(format!("→ {addr}"));
    }
    let mut counts = Vec::new();
    if let Some(b) = blocks {
        counts.push(format!("{b} blocks"));
    }
    if flows > 0 {
        counts.push(format!("{flows} flows"));
    }
    if !counts.is_empty() {
        parts.push(format!("({})", counts.join(", ")));
    }
    if let Some(addr) = addr {
        let pretty = pretty_url(addr);
        parts.push(format!("· {pretty}"));
    }
    if parts.is_empty() {
        "✓ wafer dev → ready".to_string()
    } else {
        format!("✓ wafer dev {}", parts.join(" "))
    }
}

fn pretty_url(addr: &str) -> String {
    // 0.0.0.0:8080 → http://localhost:8080
    // 127.0.0.1:3000 → http://localhost:3000
    let port = addr.rsplit(':').next().unwrap_or("");
    if port.parse::<u16>().is_ok() {
        format!("http://localhost:{port}")
    } else {
        format!("http://{addr}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_starting_event() {
        let line = r#"2026-04-30T10:00:00.000Z  INFO wafer-run: wafer runtime starting blocks=12 event="starting""#;
        assert_eq!(parse_event(line), Some(BootEvent::Starting { blocks: 12 }));
    }

    #[test]
    fn parses_flow_registered_event() {
        let line = r#"2026-04-30T10:00:00.000Z  INFO wafer-run: registered flow flow=site-main event="flow_registered""#;
        assert_eq!(
            parse_event(line),
            Some(BootEvent::FlowRegistered {
                flow: "site-main".into()
            })
        );
    }

    #[test]
    fn parses_listening_event() {
        let line = r#"2026-04-30T10:00:00.000Z  INFO wafer-run: wafer-run/http-listener listening addr=0.0.0.0:8080 event="listening""#;
        assert_eq!(
            parse_event(line),
            Some(BootEvent::Listening {
                addr: "0.0.0.0:8080".into()
            })
        );
    }

    #[test]
    fn ignores_non_wafer_lines() {
        let line = r#"2026-04-30T10:00:00.000Z  INFO some_other_crate: hello world"#;
        assert_eq!(parse_event(line), None);
    }

    #[test]
    fn banner_with_all_fields() {
        let banner = format_banner(Some(12), 3, Some("0.0.0.0:8080"));
        assert!(banner.contains("12 blocks"));
        assert!(banner.contains("3 flows"));
        assert!(banner.contains("http://localhost:8080"));
    }

    #[test]
    fn banner_with_missing_fields() {
        let banner = format_banner(None, 0, None);
        assert_eq!(banner, "✓ wafer dev → ready");
    }

    #[test]
    fn pretty_url_strips_bind_addr() {
        assert_eq!(pretty_url("0.0.0.0:8080"), "http://localhost:8080");
        assert_eq!(pretty_url("127.0.0.1:3000"), "http://localhost:3000");
    }
}
