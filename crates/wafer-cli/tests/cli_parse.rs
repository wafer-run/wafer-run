//! Unit tests for the `wafer yank` target-string parser
//! (`{org}/{block}@{version}`).

/// Test parsing of the yank target format: org/block@version
fn parse_target(s: &str) -> Result<(String, String, String), String> {
    let (org, rest) = s
        .split_once('/')
        .ok_or_else(|| "target must be org/block@version".to_string())?;
    let (block, version) = rest
        .split_once('@')
        .ok_or_else(|| "target must be org/block@version".to_string())?;
    Ok((org.to_string(), block.to_string(), version.to_string()))
}

#[test]
fn parse_target_valid() {
    assert_eq!(
        parse_target("acme/widget@0.1.0"),
        Ok(("acme".into(), "widget".into(), "0.1.0".into()))
    );
}

#[test]
fn parse_target_no_slash() {
    assert!(parse_target("acme").is_err());
}

#[test]
fn parse_target_no_at() {
    assert!(parse_target("acme/widget").is_err());
}

#[test]
fn parse_target_empty_version() {
    // Empty version is parsed (just results in empty string)
    assert_eq!(
        parse_target("acme/widget@"),
        Ok(("acme".into(), "widget".into(), "".into()))
    );
}

#[test]
fn parse_target_multiple_ats() {
    // Multiple @ signs: everything after first @ is the version
    assert_eq!(
        parse_target("acme/widget@0.1.0@extra"),
        Ok(("acme".into(), "widget".into(), "0.1.0@extra".into()))
    );
}
