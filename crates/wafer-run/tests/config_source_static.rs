//! StaticConfigSource: the in-memory ConfigSource used by tests.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §2

use std::collections::HashMap;

use wafer_block::ConfigVar;
use wafer_run::{ConfigError, ConfigSource, EnvBlockConfig, StaticConfigSource};

/// Build a ConfigVar. `required` = `!optional` in the actual ConfigVar type.
fn key(k: &str, default: &str, required: bool) -> ConfigVar {
    let mut v = ConfigVar::new(k, "test", default);
    if !required {
        v = v.optional();
    }
    v
}

#[tokio::test]
async fn loads_declared_keys_present_in_map() {
    let mut data = HashMap::new();
    data.insert(
        "SUPPERS_AI__AUTH__JWT_SECRET".to_string(),
        "secret".to_string(),
    );
    let src = StaticConfigSource::new(data);

    let declared = vec![key("SUPPERS_AI__AUTH__JWT_SECRET", "", true)];
    let cfg: EnvBlockConfig = src
        .load_for_block("suppers-ai/auth", &declared)
        .await
        .expect("load_for_block should succeed");

    assert_eq!(cfg.get("SUPPERS_AI__AUTH__JWT_SECRET"), Some("secret"));
}

#[tokio::test]
async fn missing_required_key_returns_config_error() {
    let src = StaticConfigSource::new(HashMap::new());
    let declared = vec![key("SUPPERS_AI__AUTH__JWT_SECRET", "", true)];

    let err = src
        .load_for_block("suppers-ai/auth", &declared)
        .await
        .expect_err("missing required key should error");

    match err {
        ConfigError::MissingRequired { block, key } => {
            assert_eq!(block, "suppers-ai/auth");
            assert_eq!(key, "SUPPERS_AI__AUTH__JWT_SECRET");
        }
        other => panic!("expected MissingRequired, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_optional_key_returns_default() {
    let src = StaticConfigSource::new(HashMap::new());
    let declared = vec![key("SUPPERS_AI__AUTH__SESSION_TTL", "3600", false)];

    let cfg = src
        .load_for_block("suppers-ai/auth", &declared)
        .await
        .expect("optional missing key with default should succeed");

    assert_eq!(cfg.get("SUPPERS_AI__AUTH__SESSION_TTL"), Some("3600"));
}

#[tokio::test]
async fn ignores_keys_not_declared_by_block() {
    let mut data = HashMap::new();
    data.insert(
        "SUPPERS_AI__AUTH__JWT_SECRET".to_string(),
        "secret".to_string(),
    );
    data.insert("SUPPERS_AI__OTHER__KEY".to_string(), "v".to_string());
    let src = StaticConfigSource::new(data);

    let declared = vec![key("SUPPERS_AI__AUTH__JWT_SECRET", "", true)];
    let cfg = src
        .load_for_block("suppers-ai/auth", &declared)
        .await
        .expect("load_for_block should succeed");

    assert_eq!(cfg.get("SUPPERS_AI__AUTH__JWT_SECRET"), Some("secret"));
    assert_eq!(cfg.get("SUPPERS_AI__OTHER__KEY"), None);
}

#[tokio::test]
async fn optional_key_no_default_absent_returns_none() {
    let src = StaticConfigSource::new(HashMap::new());
    let declared = vec![key("SUPPERS_AI__AUTH__OPTIONAL_TOKEN", "", false)];

    let cfg = src
        .load_for_block("suppers-ai/auth", &declared)
        .await
        .expect("optional missing key with empty default must succeed");

    assert_eq!(cfg.get("SUPPERS_AI__AUTH__OPTIONAL_TOKEN"), None);
}
