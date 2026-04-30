//! Cargo.toml inspection: figure out which binary `wafer dev` should run.

use std::path::Path;

use anyhow::{anyhow, bail, Result};

/// Outcome of binary detection.
pub enum DetectedBin {
    /// Exactly one bin candidate; use this name.
    One(String),
    /// Multiple bins; the user must pass `--bin`.
    Multiple(Vec<String>),
    /// No bin found at all.
    None,
}

/// Inspect a Cargo.toml at `manifest_path` and report what bin targets exist.
///
/// Considers explicit `[[bin]]` entries plus the implicit `src/main.rs` (whose
/// bin name is the package name).
pub fn detect_bins(manifest_path: &Path) -> Result<DetectedBin> {
    let toml_text = std::fs::read_to_string(manifest_path)
        .map_err(|e| anyhow!("failed to read {}: {e}", manifest_path.display()))?;
    detect_bins_from_str(
        &toml_text,
        manifest_path
            .parent()
            .ok_or_else(|| anyhow!("Cargo.toml has no parent dir"))?,
    )
}

pub(crate) fn detect_bins_from_str(toml_text: &str, crate_dir: &Path) -> Result<DetectedBin> {
    let doc: toml::Value =
        toml::from_str(toml_text).map_err(|e| anyhow!("Cargo.toml is not valid TOML: {e}"))?;

    let mut bins: Vec<String> = Vec::new();

    // Explicit [[bin]] entries
    if let Some(bin_array) = doc.get("bin").and_then(|v| v.as_array()) {
        for entry in bin_array {
            if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
                bins.push(name.to_string());
            }
        }
    }

    // Implicit src/main.rs → bin named after the package
    if crate_dir.join("src/main.rs").exists() {
        if let Some(pkg_name) = doc
            .get("package")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
        {
            if !bins.iter().any(|b| b == pkg_name) {
                bins.push(pkg_name.to_string());
            }
        }
    }

    Ok(match bins.len() {
        0 => DetectedBin::None,
        1 => DetectedBin::One(bins.into_iter().next().unwrap()),
        _ => DetectedBin::Multiple(bins),
    })
}

/// Resolve `--bin` argument against detection result. Errors with a friendly
/// message if the user needs to disambiguate or if no bin was found.
pub fn resolve_bin(detected: DetectedBin, user_bin: Option<&str>) -> Result<String> {
    match (detected, user_bin) {
        (_, Some(name)) => Ok(name.to_string()),
        (DetectedBin::One(name), None) => Ok(name),
        (DetectedBin::Multiple(names), None) => {
            bail!(
                "multiple bin targets found ({}); pass --bin <name>",
                names.join(", ")
            )
        }
        (DetectedBin::None, None) => bail!(
            "no bin target found in Cargo.toml — wafer dev requires a [[bin]] crate or src/main.rs"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fake_dir() -> PathBuf {
        // Returns a path that won't exist; tests don't touch the FS for the
        // src/main.rs check unless explicitly arranged.
        PathBuf::from("/nonexistent-test-dir")
    }

    #[test]
    fn explicit_single_bin_is_one() {
        let toml = r#"
            [package]
            name = "demo"
            [[bin]]
            name = "demo-bin"
        "#;
        match detect_bins_from_str(toml, &fake_dir()).unwrap() {
            DetectedBin::One(n) => assert_eq!(n, "demo-bin"),
            _ => panic!("expected One"),
        }
    }

    #[test]
    fn explicit_multiple_bins_is_multiple() {
        let toml = r#"
            [package]
            name = "demo"
            [[bin]]
            name = "a"
            [[bin]]
            name = "b"
        "#;
        match detect_bins_from_str(toml, &fake_dir()).unwrap() {
            DetectedBin::Multiple(ns) => assert_eq!(ns, vec!["a", "b"]),
            _ => panic!("expected Multiple"),
        }
    }

    #[test]
    fn no_bins_no_main_is_none() {
        let toml = r#"
            [package]
            name = "demo"
        "#;
        match detect_bins_from_str(toml, &fake_dir()).unwrap() {
            DetectedBin::None => {}
            _ => panic!("expected None"),
        }
    }

    #[test]
    fn resolve_user_override_wins() {
        let detected = DetectedBin::Multiple(vec!["a".into(), "b".into()]);
        let resolved = resolve_bin(detected, Some("b")).unwrap();
        assert_eq!(resolved, "b");
    }

    #[test]
    fn resolve_multiple_without_arg_errors() {
        let detected = DetectedBin::Multiple(vec!["a".into(), "b".into()]);
        let err = resolve_bin(detected, None).unwrap_err().to_string();
        assert!(err.contains("multiple bin targets"));
        assert!(err.contains("a, b"));
    }

    #[test]
    fn resolve_none_without_arg_errors() {
        let err = resolve_bin(DetectedBin::None, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no bin target found"));
    }
}
