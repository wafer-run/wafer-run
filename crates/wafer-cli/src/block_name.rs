//! Shared parsing for the 2-segment `{org}/{block}` block-name grammar.
//!
//! Every CLI surface that accepts a block name (scaffold, install, info,
//! yank, …) funnels through this module so the grammar is defined exactly
//! once — same value, same format everywhere.

use anyhow::{bail, Result};

/// Parse `org/block` — exactly one `/`, both segments non-empty.
/// Surrounding whitespace is trimmed.
pub fn parse_org_block(name: &str) -> Result<(String, String)> {
    let name = name.trim();
    let parsed = name
        .split_once('/')
        .filter(|(org, block)| !org.is_empty() && !block.is_empty() && !block.contains('/'));
    let Some((org, block)) = parsed else {
        bail!(
            "invalid block name {name:?}: must be in {{org}}/{{block}} format (exactly one \"/\")"
        );
    };
    Ok((org.to_string(), block.to_string()))
}

/// Parse `org/block` or `org/block@version`. Extra `/`, missing segments,
/// empty versions, or stray whitespace are rejected with a user-friendly
/// error.
pub fn parse_target(target: &str) -> Result<(String, String, Option<String>)> {
    let target = target.trim();
    let (left, version) = match target.split_once('@') {
        Some((l, v)) if !v.is_empty() => (l, Some(v.to_string())),
        Some((_, _)) => bail!("target must be org/block or org/block@version"),
        None => (target, None),
    };
    let (org, block) = parse_org_block(left)
        .map_err(|_| anyhow::anyhow!("target must be org/block or org/block@version"))?;
    Ok((org, block, version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_org_block_accepts_valid() {
        assert_eq!(
            parse_org_block("acme/widget").unwrap(),
            ("acme".into(), "widget".into())
        );
    }

    #[test]
    fn parse_org_block_trims_whitespace() {
        assert_eq!(
            parse_org_block("  acme/widget  ").unwrap(),
            ("acme".into(), "widget".into())
        );
    }

    #[test]
    fn parse_org_block_rejects_missing_slash() {
        assert!(parse_org_block("justname").is_err());
    }

    #[test]
    fn parse_org_block_rejects_empty_segments_and_extra_slash() {
        assert!(parse_org_block("/b").is_err());
        assert!(parse_org_block("a/").is_err());
        assert!(parse_org_block("a/b/c").is_err());
        assert!(parse_org_block("").is_err());
        assert!(parse_org_block("/").is_err());
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
    fn parse_target_rejects_interior_empty_segments() {
        // Leading slash, empty middle segment, bare slash, empty string.
        assert!(parse_target("/widget").is_err());
        assert!(parse_target("acme//widget").is_err());
        assert!(parse_target("/").is_err());
        assert!(parse_target("").is_err());
    }

    #[test]
    fn parse_target_trims_whitespace() {
        let (o, b, v) = parse_target("  acme/widget  ").unwrap();
        assert_eq!((o.as_str(), b.as_str(), v), ("acme", "widget", None));
    }

    #[test]
    fn parse_target_rejects_empty_version() {
        assert!(parse_target("acme/widget@").is_err());
    }

    #[test]
    fn parse_target_error_message_is_user_friendly() {
        let err = parse_target("acme").unwrap_err().to_string();
        assert!(err.contains("target must be org/block"), "{err}");
    }
}
