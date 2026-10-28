//! Workspace validator: every block's declared config keys obey the naming
//! convention for the slot they live in, no key is declared in both slots,
//! and every key carries a sentence-case label. Enforces the 2026-05-15
//! flow-config-api spec and the label rule on
//! [`ConfigVar::name`](wafer_block::ConfigVar::name).

mod declared_config;

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

/// The proper nouns a label may capitalise after its first word, each
/// spelled as its owner spells it. No wafer-run label names one. The list is
/// the same as impresspress's `test_support::case`, so a label is judged the
/// same in both repos: their labels sit side by side in impresspress's
/// Variables table. Change it in both, and only with a label that needs it.
const PROPER_NOUNS: &[&str] = &["OAuth"];

/// Whether `label` capitalises its first word and only that, plus any
/// acronym and any of [`PROPER_NOUNS`]: "URL prefix", "SPA mode", "STUN
/// server URLs" and "Enable OAuth" are; "Max Age (seconds)", "Max age
/// (Seconds)", "Read-Only" and "max age" are not. An empty label is not.
///
/// Words are split at whitespace and hyphens, so the second half of a
/// hyphenated word is a word of its own, and are read from their first
/// letter, so a parenthesised word is checked like any other. An acronym is
/// a word with no lowercase letter, or one whose only lowercase letter is a
/// plural `s` ("URLs").
///
/// The same rule as impresspress's `impresspress_core::test_support::case`,
/// which checks impresspress's own labels.
fn is_sentence_case(label: &str) -> bool {
    let first_is_capital = label.chars().next().is_some_and(|c| !c.is_lowercase());
    first_is_capital
        && label
            .split(|c: char| c.is_whitespace() || c == '-')
            .skip(1)
            .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()))
            .all(|word| {
                let starts_upper = word.chars().next().is_some_and(char::is_uppercase);
                !starts_upper || is_acronym(word) || PROPER_NOUNS.contains(&word)
            })
}

/// A word with no lowercase letter, or with a plural `s` as its only one.
fn is_acronym(word: &str) -> bool {
    let all_capitals = |w: &str| !w.chars().any(char::is_lowercase);
    all_capitals(word)
        || word
            .strip_suffix('s')
            .is_some_and(|stem| stem.chars().count() >= 2 && all_capitals(stem))
}

#[test]
fn all_blocks_obey_config_key_naming() {
    let mut failures: Vec<String> = Vec::new();
    let mut count = 0usize;

    for (_, block) in declared_config::every_block() {
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
        for (slot, cv) in info
            .config_keys
            .iter()
            .map(|cv| ("config_keys", cv))
            .chain(info.flow_config.iter().map(|cv| ("flow_config", cv)))
        {
            if !is_sentence_case(&cv.name) {
                failures.push(format!(
                    "{}: {slot} entry {:?} has label {:?}, which is not sentence \
                     case (capitalise the first word only; acronyms and \
                     {PROPER_NOUNS:?} keep their capitals)",
                    info.name, cv.key, cv.name
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

#[test]
fn sentence_case_validator_examples() {
    for ok in [
        "URL prefix",
        "SPA mode",
        "CSP",
        "Read-only",
        "Allow blob: workers",
        "IPv6 prefix",
        "Max age (seconds)",
        "Connect timeout (s)",
        "STUN server URLs",
        "Enable OAuth",
        "JWT secret",
    ] {
        assert!(is_sentence_case(ok), "{ok}");
    }
    for bad in [
        "",
        "URL Prefix",
        "Max Age (seconds)",
        "Max age (Seconds)",
        "Read-Only",
        "Statement Budget",
        "max age",
        "Is As",
    ] {
        assert!(!is_sentence_case(bad), "{bad}");
    }
}
