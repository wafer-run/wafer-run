//! `wafer search <query> [--json]` — package discovery.
//!
//! Read-only, anonymous. Empty query is rejected locally (does not hit the
//! server). Default output is a table aligned by longest entry; `--json`
//! emits `{"packages": [...]}` passing `PackageSummary` rows through.

use anyhow::{bail, Result};

use crate::registry_client::{self, PackageSummary};

/// Width for non-TTY output per spec.
const DEFAULT_WIDTH: usize = 80;

fn term_width() -> usize {
    terminal_size::terminal_size()
        .map(|(terminal_size::Width(w), _)| w as usize)
        .unwrap_or(DEFAULT_WIDTH)
}

/// Render a non-empty list of summaries as a three-column table.
/// SUMMARY is truncated (with a trailing ellipsis) to fit `width`.
pub(crate) fn render_table(packages: &[PackageSummary], width: usize) -> String {
    const HEADER_NAME: &str = "ORG/BLOCK";
    const HEADER_LATEST: &str = "LATEST";
    const HEADER_SUMMARY: &str = "SUMMARY";
    const GAP: usize = 2;

    let name_width = packages
        .iter()
        .map(|p| p.org.len() + 1 + p.name.len())
        .max()
        .unwrap_or(0)
        .max(HEADER_NAME.len());
    let latest_width = packages
        .iter()
        .map(|p| p.latest.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(0)
        .max(HEADER_LATEST.len());

    // Summary column gets whatever's left, with a legibility floor equal to
    // the header label ("SUMMARY"). At very narrow widths the full line may
    // exceed `width` — a narrow terminal trades overflow for readability.
    let summary_budget = width
        .saturating_sub(name_width + GAP + latest_width + GAP)
        .max(HEADER_SUMMARY.len());

    let mut out = String::new();
    out.push_str(&format!(
        "{HEADER_NAME:<name_width$}  {HEADER_LATEST:<latest_width$}  {HEADER_SUMMARY}\n"
    ));
    for p in packages {
        let name = format!("{}/{}", p.org, p.name);
        let latest = p.latest.as_deref().unwrap_or("-");
        let summary = truncate(p.summary.as_deref().unwrap_or(""), summary_budget);
        out.push_str(&format!(
            "{name:<name_width$}  {latest:<latest_width$}  {summary}\n"
        ));
    }
    out
}

/// Truncate `s` to `max` chars, appending `…` if truncation occurred.
/// If `max < 1`, returns empty. If `max == 1` and truncation needed,
/// returns `"…"`.
///
/// Note: truncation is codepoint-based, not grapheme-based. Wide glyphs
/// (CJK, emoji, ZWJ sequences) may visually exceed the nominal budget or
/// split across a grapheme boundary. Accepted for package-summary use.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    if max == 1 {
        return "…".to_string();
    }
    let mut out: String = chars.iter().take(max - 1).collect();
    out.push('…');
    out
}

pub async fn run(query: String, json: bool, registry: Option<String>) -> Result<()> {
    if query.is_empty() {
        bail!("search requires a non-empty query");
    }
    let url = registry_client::resolve_registry(registry);
    let resp = registry_client::search(&url, &query).await?;

    if json {
        let body = serde_json::json!({ "packages": resp.packages });
        println!("{}", serde_json::to_string(&body)?);
        return Ok(());
    }

    if resp.packages.is_empty() {
        println!("no matches for '{query}'");
        return Ok(());
    }

    let width = term_width();
    print!("{}", render_table(&resp.packages, width));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(org: &str, name: &str, summary: Option<&str>, latest: Option<&str>) -> PackageSummary {
        PackageSummary {
            org: org.into(),
            name: name.into(),
            summary: summary.map(Into::into),
            latest: latest.map(Into::into),
        }
    }

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long_string_gets_ellipsis() {
        assert_eq!(truncate("hello world", 8), "hello w…");
    }

    #[test]
    fn truncate_max_zero_is_empty() {
        assert_eq!(truncate("anything", 0), "");
    }

    #[test]
    fn truncate_max_one_is_ellipsis() {
        assert_eq!(truncate("hello", 1), "…");
    }

    #[test]
    fn render_table_aligns_by_longest_name() {
        let rows = vec![
            pkg("a", "short", Some("S1"), Some("1.0")),
            pkg("acme", "widget", Some("Longer summary"), Some("0.3.1")),
        ];
        let out = render_table(&rows, 80);
        let lines: Vec<&str> = out.lines().collect();
        // Header then two rows.
        assert_eq!(lines.len(), 3);
        // Both rows start at column 0 with left-aligned names of equal width.
        // Longest name is "acme/widget" (11 chars).
        assert!(
            lines[1].starts_with("a/short    "),
            "row1 not padded: {:?}",
            lines[1]
        );
        assert!(
            lines[2].starts_with("acme/widget  "),
            "row2 not padded: {:?}",
            lines[2]
        );
    }

    #[test]
    fn render_table_truncates_summary_to_fit_width() {
        let rows = vec![pkg(
            "a",
            "b",
            Some("This is a summary that is way too long for a tiny terminal"),
            Some("1.0.0"),
        )];
        // Width 30: name col = max(9,3)=9, latest col = max(6,5)=6. gap 2+2=4. Summary budget = 30-9-6-4 = 11.
        let out = render_table(&rows, 30);
        let last_line = out.lines().nth(1).unwrap();
        // Summary after the latest col — expect the truncated form ending in '…'.
        assert!(last_line.ends_with("…"), "{last_line}");
    }

    #[test]
    fn render_table_handles_missing_summary_and_latest() {
        let rows = vec![pkg("x", "y", None, None)];
        let out = render_table(&rows, 80);
        let row = out.lines().nth(1).unwrap();
        assert!(row.contains("x/y"), "{row}");
        assert!(
            row.contains(" - "),
            "expected dash placeholder for missing latest: {row}"
        );
    }
}
