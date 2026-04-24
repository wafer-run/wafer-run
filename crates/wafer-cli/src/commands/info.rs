//! `wafer info <org>/<block>[@<ver>] [--json] [--all]` — package / version detail.
//!
//! Without `@ver`: package form — summary, latest, version count with
//! yanked tally, up to 5 most-recent versions (or all with `--all`).
//!
//! With `@ver`: version form — publish date, abi, size, sha, license,
//! download URL, install hint. Yanked versions render a leading banner.
//!
//! `--json` emits `{"package": <PackageDetail>}` or `{"version": <VersionDetail>}`.

use anyhow::{bail, Result};

use crate::registry_client::{self, PackageDetail, VersionDetail, VersionSummary};

/// Parse `org/block` or `org/block@version`. Extra `/` or missing segments
/// are rejected with a user-friendly error.
pub(crate) fn parse_target(target: &str) -> Result<(String, String, Option<String>)> {
    let (left, version) = match target.split_once('@') {
        Some((l, v)) if !v.is_empty() => (l, Some(v.to_string())),
        Some((_, _)) => bail!("target must be org/block or org/block@version"),
        None => (target, None),
    };
    let mut parts = left.split('/');
    let org = parts.next().unwrap_or("");
    let block = parts.next().unwrap_or("");
    if org.is_empty() || block.is_empty() || parts.next().is_some() {
        bail!("target must be org/block or org/block@version");
    }
    Ok((org.to_string(), block.to_string(), version))
}

/// Format a unix-epoch seconds timestamp as ISO date (`YYYY-MM-DD`).
/// Invalid values (negative / far future) fall back to the raw number.
pub(crate) fn format_date(epoch_secs: i64) -> String {
    chrono::DateTime::from_timestamp(epoch_secs, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| epoch_secs.to_string())
}

/// Format a byte count as an IEC-style string: `1023`, `1.0 KiB`, `1.2 MiB`.
pub(crate) fn format_size(bytes: i64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 0 {
        return bytes.to_string();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Render the package view. `all=false` caps recent versions at 5.
pub(crate) fn render_package(pkg: &PackageDetail, all: bool) -> String {
    let mut versions = pkg.versions.clone();
    // Newest first by published_at desc; stable — preserve input order on ties.
    versions.sort_by(|a, b| b.published_at.cmp(&a.published_at));

    let yanked_count = versions.iter().filter(|v| v.yanked != 0).count();
    let latest = versions
        .iter()
        .find(|v| v.yanked == 0)
        .map(|v| v.version.as_str())
        .unwrap_or("-");

    let mut out = String::new();
    out.push_str(&format!("{}/{}\n", pkg.org, pkg.name));
    out.push_str(&format!(
        "  summary:  {}\n",
        pkg.summary.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!("  latest:   {latest}\n"));
    out.push_str(&format!(
        "  versions: {}  ({} yanked)\n\n",
        versions.len(),
        yanked_count
    ));

    out.push_str("Recent versions:\n");
    let slice: &[VersionSummary] = if all {
        &versions
    } else {
        let end = versions.len().min(5);
        &versions[..end]
    };
    for v in slice {
        let yanked_mark = if v.yanked != 0 { " (yanked)" } else { "" };
        out.push_str(&format!(
            "  {}  {}   abi={}   {}{}\n",
            v.version,
            format_date(v.published_at),
            v.abi,
            format_size(v.size_bytes),
            yanked_mark,
        ));
    }
    out
}

/// Render the version view. `registry` is the resolved base URL for the
/// download hint; trailing slash trimmed.
pub(crate) fn render_version(v: &VersionDetail, registry: &str) -> String {
    let mut out = String::new();
    if v.yanked != 0 {
        out.push_str("⚠ THIS VERSION IS YANKED\n");
    }
    out.push_str(&format!("{}/{}@{}\n", v.org_name, v.pkg_name, v.version));
    out.push_str(&format!("  published:   {}\n", format_date(v.published_at)));
    out.push_str(&format!("  abi:         {}\n", v.abi));
    out.push_str(&format!("  size:        {}\n", format_size(v.size_bytes)));
    out.push_str(&format!("  sha256:      {}\n", v.sha256));
    out.push_str(&format!(
        "  license:     {}\n",
        v.license.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "  download:    {}/registry/download/{}/{}/{}.wafer\n",
        registry.trim_end_matches('/'),
        v.org_name,
        v.pkg_name,
        v.version,
    ));
    out.push_str(&format!(
        "  install:     wafer install {}/{}@{}\n",
        v.org_name, v.pkg_name, v.version
    ));
    out
}

pub async fn run(target: String, json: bool, all: bool, registry: Option<String>) -> Result<()> {
    let (org, block, version) = parse_target(&target)?;
    let url = registry_client::resolve_registry(registry);

    match version {
        None => {
            let pkg = registry_client::get_package(&url, &org, &block).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({ "package": pkg }))?
                );
            } else {
                print!("{}", render_package(&pkg, all));
            }
        }
        Some(ver) => {
            let v = registry_client::get_version(&url, &org, &block, &ver).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({ "version": v }))?
                );
            } else {
                print!("{}", render_version(&v, &url));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vs(version: &str, published_at: i64, yanked: i64) -> VersionSummary {
        VersionSummary {
            version: version.into(),
            abi: 1,
            sha256: "deadbeef".into(),
            size_bytes: 120_300,
            license: Some("Apache-2.0".into()),
            yanked,
            published_at,
        }
    }

    #[test]
    fn parse_target_package_form() {
        let (o, b, v) = parse_target("acme/widget").unwrap();
        assert_eq!((o.as_str(), b.as_str(), v), ("acme", "widget", None));
    }

    #[test]
    fn parse_target_version_form() {
        let (o, b, v) = parse_target("acme/widget@0.3.1").unwrap();
        assert_eq!(
            (o.as_str(), b.as_str(), v.as_deref()),
            ("acme", "widget", Some("0.3.1"))
        );
    }

    #[test]
    fn parse_target_rejects_missing_block() {
        assert!(parse_target("acme").is_err());
        assert!(parse_target("acme/").is_err());
    }

    #[test]
    fn parse_target_rejects_too_many_segments() {
        assert!(parse_target("acme/widget/sub").is_err());
    }

    #[test]
    fn parse_target_rejects_empty_version() {
        assert!(parse_target("acme/widget@").is_err());
    }

    #[test]
    fn format_date_formats_epoch_as_iso() {
        // 2026-04-12 00:00:00 UTC = 1_775_952_000
        assert_eq!(format_date(1_775_952_000), "2026-04-12");
    }

    #[test]
    fn format_size_renders_iec_units() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(120_300), "117.5 KiB");
        assert_eq!(format_size(1_500_000), "1.4 MiB");
    }

    #[test]
    fn render_package_caps_at_5_when_not_all() {
        let pkg = PackageDetail {
            org: "acme".into(),
            name: "widget".into(),
            summary: Some("A widget.".into()),
            versions: (0..8)
                .map(|i| vs(&format!("0.{i}.0"), 1_700_000_000 + i, 0))
                .collect(),
        };
        let out = render_package(&pkg, false);
        // Header (4 info lines + blank) + "Recent versions:" + 5 versions.
        let version_lines = out
            .lines()
            .filter(|l| l.trim_start().starts_with("0."))
            .count();
        assert_eq!(version_lines, 5);
    }

    #[test]
    fn render_package_shows_all_with_all_flag() {
        let pkg = PackageDetail {
            org: "acme".into(),
            name: "widget".into(),
            summary: Some("A widget.".into()),
            versions: (0..8)
                .map(|i| vs(&format!("0.{i}.0"), 1_700_000_000 + i, 0))
                .collect(),
        };
        let out = render_package(&pkg, true);
        let version_lines = out
            .lines()
            .filter(|l| l.trim_start().starts_with("0."))
            .count();
        assert_eq!(version_lines, 8);
    }

    #[test]
    fn render_package_counts_yanked_and_picks_latest_non_yanked() {
        let pkg = PackageDetail {
            org: "acme".into(),
            name: "widget".into(),
            summary: None,
            versions: vec![
                vs("0.4.0", 4, 1), // yanked, newest
                vs("0.3.1", 3, 0),
                vs("0.3.0", 2, 0),
            ],
        };
        let out = render_package(&pkg, false);
        assert!(out.contains("versions: 3  (1 yanked)"), "{out}");
        assert!(out.contains("latest:   0.3.1"), "{out}");
    }

    #[test]
    fn render_version_without_yanked_has_no_banner() {
        let v = VersionDetail {
            org_name: "acme".into(),
            pkg_name: "widget".into(),
            version: "0.3.1".into(),
            abi: 1,
            sha256: "deadbeef".into(),
            storage_key: "k".into(),
            size_bytes: 120_300,
            license: Some("Apache-2.0".into()),
            readme_md: None,
            dependencies: None,
            capabilities: None,
            yanked: 0,
            yanked_reason: None,
            published_at: 1_775_952_000,
        };
        let out = render_version(&v, "https://wafer.run");
        assert!(!out.contains("YANKED"), "{out}");
        assert!(out.contains("acme/widget@0.3.1"), "{out}");
        assert!(out.contains("published:   2026-04-12"), "{out}");
        assert!(out.contains("sha256:      deadbeef"), "{out}");
        assert!(
            out.contains(
                "download:    https://wafer.run/registry/download/acme/widget/0.3.1.wafer"
            ),
            "{out}"
        );
        assert!(
            out.contains("install:     wafer install acme/widget@0.3.1"),
            "{out}"
        );
    }

    #[test]
    fn render_version_yanked_shows_banner() {
        let v = VersionDetail {
            org_name: "acme".into(),
            pkg_name: "widget".into(),
            version: "0.3.1".into(),
            abi: 1,
            sha256: "d".into(),
            storage_key: "k".into(),
            size_bytes: 1,
            license: None,
            readme_md: None,
            dependencies: None,
            capabilities: None,
            yanked: 1,
            yanked_reason: Some("bad".into()),
            published_at: 0,
        };
        let out = render_version(&v, "https://wafer.run");
        assert!(out.starts_with("⚠ THIS VERSION IS YANKED"), "{out}");
    }
}
