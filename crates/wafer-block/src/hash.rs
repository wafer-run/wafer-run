//! Hashing utilities.

use sha2::Digest;

/// Encode bytes as lowercase hex string.
pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        result.push(HEX[(b >> 4) as usize] as char);
        result.push(HEX[(b & 0xf) as usize] as char);
    }
    result
}

/// Compute SHA-256 and return as hex string. Used for deterministic hashing (API keys, etc.).
pub fn sha256_hex(data: &[u8]) -> String {
    hex_encode(&sha256(data))
}

/// SHA-256 hash.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Expand environment variables in the format `$VAR` or `${VAR}`.
///
/// This is a *lossless* text transform: anything that isn't a well-formed,
/// defined variable reference is preserved exactly as written rather than
/// being dropped or blanked out. Specifically:
///
/// - A literal `$` that doesn't start a `$VAR`/`${VAR}` reference (e.g.
///   trailing at end-of-string, or followed by punctuation/whitespace) is
///   copied through unchanged.
/// - `$$` is an escape for a single literal `$` (consumes both chars, emits
///   one `$`, and does not attempt variable expansion).
/// - `${VAR` with no closing `}` is treated as literal text (the `${` plus
///   whatever was scanned is copied through unchanged), not consumed to
///   end-of-string.
/// - A reference to an *undefined* variable (`$VAR` or `${VAR}`) is left in
///   the output as its original literal text (not substituted with an empty
///   string); a `tracing::warn!` is still emitted so operators can spot the
///   miss.
/// - A reference to a *defined* variable expands normally, in both the
///   bare `$VAR` and braced `${VAR}` forms.
///
/// This matters because callers (e.g. `registry.rs`) run this over whole
/// config files before parsing — any lossy path here silently corrupts
/// operator config/secrets that happen to contain a `$` (passwords,
/// regexes, prices, etc).
///
/// Native-only: requires `std::env::var`.
#[cfg(not(target_arch = "wasm32"))]
pub fn expand_env_vars(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '$' {
            result.push(c);
            continue;
        }

        match chars.peek() {
            Some(&'$') => {
                // "$$" escapes to a single literal "$".
                chars.next();
                result.push('$');
            }
            Some(&'{') => {
                chars.next(); // consume '{'
                let mut var_name = String::new();
                let mut closed = false;
                while let Some(&nc) = chars.peek() {
                    if nc == '}' {
                        chars.next();
                        closed = true;
                        break;
                    }
                    var_name.push(nc);
                    chars.next();
                }
                if !closed {
                    // Unclosed "${...": no valid reference — preserve the
                    // original text verbatim instead of eating to EOF.
                    result.push_str("${");
                    result.push_str(&var_name);
                } else {
                    match std::env::var(&var_name) {
                        Ok(val) => result.push_str(&val),
                        Err(_) => {
                            tracing::warn!(var = %var_name, "undefined environment variable referenced");
                            result.push_str("${");
                            result.push_str(&var_name);
                            result.push('}');
                        }
                    }
                }
            }
            Some(&nc) if nc.is_alphanumeric() || nc == '_' => {
                let mut var_name = String::new();
                while let Some(&nc2) = chars.peek() {
                    if nc2.is_alphanumeric() || nc2 == '_' {
                        var_name.push(nc2);
                        chars.next();
                    } else {
                        break;
                    }
                }
                match std::env::var(&var_name) {
                    Ok(val) => result.push_str(&val),
                    Err(_) => {
                        tracing::warn!(var = %var_name, "undefined environment variable referenced");
                        result.push('$');
                        result.push_str(&var_name);
                    }
                }
            }
            _ => {
                // Bare '$' that doesn't start a $VAR/${VAR} reference
                // (end-of-string, or followed by punctuation/whitespace) —
                // preserve it literally.
                result.push('$');
            }
        }
    }

    result
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;

    // These tests mutate process-wide environment variables. `cargo test`
    // runs tests for a crate in one process (on separate threads by
    // default), so each test uses a unique var name to avoid cross-test
    // races.

    #[test]
    fn bare_dollar_not_forming_a_var_is_preserved() {
        // '$' followed by a space is not a valid $VAR/${VAR} — must survive
        // untouched rather than being silently dropped.
        assert_eq!(expand_env_vars("cost is $ (unset)"), "cost is $ (unset)");
    }

    #[test]
    fn trailing_dollar_is_preserved() {
        assert_eq!(expand_env_vars("cost is 5$"), "cost is 5$");
    }

    #[test]
    fn dollar_dollar_escapes_to_single_literal_dollar() {
        assert_eq!(expand_env_vars("pa$$word"), "pa$word");
    }

    #[test]
    fn dollar_dollar_before_defined_var_does_not_expand() {
        // `$$` must emit a literal `$` and NOT attempt to expand what follows.
        // Using a DEFINED var proves no lookup happens — an "expand-then-fallback"
        // regression would substitute the value here and this test would catch it.
        std::env::set_var("WR4_DOLLAR_SET", "should-not-appear");
        assert_eq!(expand_env_vars("$$WR4_DOLLAR_SET"), "$WR4_DOLLAR_SET");
        std::env::remove_var("WR4_DOLLAR_SET");
    }

    #[test]
    fn undefined_braced_var_is_left_literal_not_blanked() {
        std::env::remove_var("WR4_NOPE_BRACED");
        assert_eq!(
            expand_env_vars("x=${WR4_NOPE_BRACED}"),
            "x=${WR4_NOPE_BRACED}"
        );
    }

    #[test]
    fn undefined_bare_var_is_left_literal_not_blanked() {
        std::env::remove_var("WR4_NOPE_BARE");
        assert_eq!(expand_env_vars("x=$WR4_NOPE_BARE!"), "x=$WR4_NOPE_BARE!");
    }

    #[test]
    fn unclosed_brace_is_preserved_literal() {
        assert_eq!(expand_env_vars("x=${abc"), "x=${abc");
    }

    #[test]
    fn defined_braced_var_still_expands() {
        std::env::set_var("WR4_BRACED", "braced-val");
        assert_eq!(expand_env_vars("x=${WR4_BRACED}"), "x=braced-val");
        std::env::remove_var("WR4_BRACED");
    }

    #[test]
    fn defined_bare_var_still_expands() {
        // No-braces $VAR form must keep expanding for a defined var — the
        // literal/escape fixes only change the lossy (undefined/malformed)
        // paths, not this happy path.
        std::env::set_var("WR4_BARE", "bare-val");
        assert_eq!(expand_env_vars("x=$WR4_BARE!"), "x=bare-val!");
        std::env::remove_var("WR4_BARE");
    }
}
