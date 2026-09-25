//! Route matching and path variable extraction.

use std::borrow::Cow;

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

/// The part of a route pattern that decides which request paths it matches:
/// the pattern with every `{name}` placeholder segment written as `{}`.
///
/// A placeholder's name only decides which `req.param.*` meta a match sets,
/// so two patterns with the same shape are matched by [`match_path`] against
/// exactly the same set of paths — `/items/{id}` and `/items/{item_id}` are
/// one route. A pattern ending in `/**` is its own shape: `match_path`
/// compares everything before the `/**` byte for byte, braces included.
pub fn route_shape(pattern: &str) -> Cow<'_, str> {
    let is_placeholder = |segment: &str| segment.starts_with('{') && segment.ends_with('}');
    if pattern.ends_with("/**") || !pattern.split('/').any(|s| is_placeholder(s) && s != "{}") {
        return Cow::Borrowed(pattern);
    }
    Cow::Owned(
        pattern
            .split('/')
            .map(|segment| {
                if is_placeholder(segment) {
                    "{}"
                } else {
                    segment
                }
            })
            .collect::<Vec<_>>()
            .join("/"),
    )
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

    #[test]
    fn route_shape_erases_placeholder_names_only() {
        assert_eq!(route_shape("/items/{id}"), route_shape("/items/{item_id}"));
        assert_eq!(route_shape("/items/{id}/tags/{tag}"), "/items/{}/tags/{}");
        assert_eq!(route_shape("/items/{key...}"), "/items/{}");
        assert_eq!(route_shape("/items"), "/items");
        assert_ne!(route_shape("/items/{id}"), route_shape("/items/new"));
        // An infix brace is a literal to `match_path`, so it stays.
        assert_eq!(route_shape("/v{n}/items"), "/v{n}/items");
    }

    #[test]
    fn route_shape_keeps_a_rest_pattern_verbatim() {
        // `match_path` compares a `/**` pattern's prefix literally, so
        // `/a/{x}/**` does not match `/a/1/b` and must not be reshaped.
        assert!(!match_path("/a/{x}/**", "/a/1/b"));
        assert_eq!(route_shape("/a/{x}/**"), "/a/{x}/**");
        assert_ne!(route_shape("/a/{x}/**"), route_shape("/a/{y}/**"));
    }
}
