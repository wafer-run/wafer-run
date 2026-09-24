//! Route matching and path variable extraction.

use crate::{meta::META_REQ_PARAM_PREFIX, Message};

/// Match a message kind pattern against a message kind.
pub fn matches_pattern(pattern: &str, message_kind: &str) -> bool {
    if pattern.is_empty() || pattern == "*" {
        return true;
    }

    if let Some(idx) = pattern.find(":/") {
        let pattern_method = &pattern[..idx];
        let pattern_path = &pattern[idx + 1..];

        let Some(msg_idx) = message_kind.find(":/") else {
            return false;
        };
        let msg_method = &message_kind[..msg_idx];
        let msg_path = &message_kind[msg_idx + 1..];

        if pattern_method != "*" && pattern_method != msg_method {
            return false;
        }

        return match_path(pattern_path, msg_path);
    }

    if pattern == message_kind {
        return true;
    }

    if let Some(prefix) = pattern.strip_suffix(".**") {
        return message_kind.len() > prefix.len()
            && message_kind.starts_with(prefix)
            && message_kind.as_bytes()[prefix.len()] == b'.';
    }

    if let Some(prefix) = pattern.strip_suffix(".*") {
        if !(message_kind.len() > prefix.len()
            && message_kind.starts_with(prefix)
            && message_kind.as_bytes()[prefix.len()] == b'.')
        {
            return false;
        }
        let rest = &message_kind[prefix.len() + 1..];
        return !rest.contains('.');
    }

    false
}

/// Extract path variables from a matched pattern and set them as req.param.{name} meta.
pub fn extract_path_vars(pattern: &str, path: &str, msg: &mut Message) {
    let pattern = pattern.strip_suffix("/**").unwrap_or(pattern);

    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let path_parts: Vec<&str> = path.split('/').collect();

    for (i, pp) in pattern_parts.iter().enumerate() {
        if i >= path_parts.len() {
            break;
        }
        if pp.starts_with('{') && pp.ends_with('}') {
            let var_name = &pp[1..pp.len() - 1];
            msg.set_meta(
                format!("{META_REQ_PARAM_PREFIX}{var_name}"),
                path_parts[i].to_string(),
            );
        }
    }
}

/// Match a request path against a route pattern.
///
/// A pattern is an exact path, a path ending in `/**` (the prefix itself or
/// anything below it), or a `/`-separated template whose `{name}` segments
/// each match exactly one non-empty path segment: `/users/{id}` matches
/// `/users/42` but not `/users/`.
pub fn match_path(pattern: &str, path: &str) -> bool {
    if pattern == path {
        return true;
    }

    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path == prefix
            || (path.len() > prefix.len()
                && path.starts_with(prefix)
                && path.as_bytes()[prefix.len()] == b'/');
    }

    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let path_parts: Vec<&str> = path.split('/').collect();

    if pattern_parts.len() != path_parts.len() {
        return false;
    }

    for (pp, actual) in pattern_parts.iter().zip(path_parts.iter()) {
        if pp.starts_with('{') && pp.ends_with('}') {
            if actual.is_empty() {
                return false;
            }
            continue;
        }
        if pp != actual {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_needs_a_non_empty_segment() {
        assert!(match_path("/users/{id}", "/users/42"));
        assert!(!match_path("/users/{id}", "/users/"));
        assert!(!match_path("/users/{id}/posts", "/users//posts"));
        assert!(!matches_pattern("GET:/users/{id}", "GET:/users/"));
    }

    #[test]
    fn literal_and_wildcard_patterns_are_unchanged() {
        assert!(match_path("/users", "/users"));
        assert!(!match_path("/users", "/users/"));
        assert!(match_path("/static/**", "/static"));
        assert!(match_path("/static/**", "/static/a/b"));
        assert!(!match_path("/static/**", "/staticfoo"));
    }
}
