//! Workspace validator: every block's declared config keys obey the naming
//! convention for the slot they live in, and no key is declared in both
//! slots. Enforces the 2026-05-15 flow-config-api spec.

// Reference each block crate that uses register_static_block! so the
// linker keeps their linkme entries alive in this test binary.
#[allow(unused_imports)]
use wafer_block_cors as _;
#[allow(unused_imports)]
use wafer_block_readonly_guard as _;
use wafer_run::static_registration::STATIC_BLOCK_REGISTRATIONS;

/// SCREAMING_SNAKE with `{ORG}__{BLOCK}__{KEY}` form.
/// First and last segments contain `[A-Z][A-Z0-9_]*` characters; the
/// `__` separator appears at least twice in total.
fn is_screaming_snake_prefixed(s: &str) -> bool {
    let parts: Vec<&str> = s.split("__").collect();
    if parts.len() < 3 {
        return false;
    }
    parts.iter().all(|part| {
        !part.is_empty()
            && part.starts_with(|c: char| c.is_ascii_uppercase())
            && part
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            && !part.contains("__") // no empty inter-segment
    })
}

/// snake_case identifier: starts with lowercase letter, then lowercase
/// letters, digits, or single underscores. No `__` separators.
fn is_snake_case(s: &str) -> bool {
    if s.is_empty() || s.contains("__") {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[test]
fn all_blocks_obey_config_key_naming() {
    let mut failures: Vec<String> = Vec::new();
    let mut count = 0usize;

    for entry in STATIC_BLOCK_REGISTRATIONS.iter() {
        let block = (entry.factory)();
        let info = block.info();
        count += 1;

        for cv in &info.config_keys {
            if !is_screaming_snake_prefixed(&cv.key) {
                failures.push(format!(
                    "{}: config_keys entry {:?} is not SCREAMING_SNAKE with \
                     `{{ORG}}__{{BLOCK}}__{{KEY}}` form",
                    info.name, cv.key
                ));
            }
        }
        for cv in &info.flow_config {
            if !is_snake_case(&cv.key) {
                failures.push(format!(
                    "{}: flow_config entry {:?} is not snake_case",
                    info.name, cv.key
                ));
            }
        }

        let env_keys: std::collections::HashSet<&str> =
            info.config_keys.iter().map(|cv| cv.key.as_str()).collect();
        let flow_keys: std::collections::HashSet<&str> =
            info.flow_config.iter().map(|cv| cv.key.as_str()).collect();
        for k in env_keys.intersection(&flow_keys) {
            failures.push(format!(
                "{}: key {:?} is declared in BOTH config_keys and flow_config",
                info.name, k
            ));
        }
    }

    assert!(count > 0, "no statically-registered blocks were enumerated");
    assert!(
        failures.is_empty(),
        "BlockInfo config-key naming violations across {count} blocks:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn screaming_snake_validator_examples() {
    assert!(is_screaming_snake_prefixed(
        "WAFER_RUN__NETWORK__MAX_RESPONSE_BYTES"
    ));
    assert!(is_screaming_snake_prefixed("WAFER_RUN__S3__ENDPOINT"));
    assert!(!is_screaming_snake_prefixed("readonly")); // not screaming
    assert!(!is_screaming_snake_prefixed("WAFER_RUN__S3")); // only one __
    assert!(!is_screaming_snake_prefixed("WAFER_RUN____S3")); // empty segment
    assert!(!is_screaming_snake_prefixed("wafer_run__s3__endpoint")); // not uppercase
}

#[test]
fn snake_case_validator_examples() {
    assert!(is_snake_case("readonly"));
    assert!(is_snake_case("allowed_origins"));
    assert!(is_snake_case("max_requests"));
    assert!(is_snake_case("web_prefix"));
    assert!(!is_snake_case("AllowedOrigins"));
    assert!(!is_snake_case("WAFER_RUN__S3__BUCKET")); // contains __
    assert!(!is_snake_case("")); // empty
    assert!(!is_snake_case("_underscore_first"));
}
