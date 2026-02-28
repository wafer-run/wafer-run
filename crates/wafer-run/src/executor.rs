use crate::config::Node;
use crate::meta::META_REQ_PARAM_PREFIX;
use crate::types::*;

/// matchesPattern checks if messageKind matches the given glob-style pattern.
///
/// Patterns:
///   - ""         -> always matches (unconditional)
///   - "*"        -> matches anything
///   - "user.*"   -> matches "user.create", "user.delete", etc. (single segment)
///   - "user.create" -> exact match
///   - "user.**"  -> matches "user.create", "user.create.done", etc. (multi-segment)
///   - "GET:/path/**" -> matches HTTP method + path
pub fn matches_pattern(pattern: &str, message_kind: &str) -> bool {
    if pattern.is_empty() || pattern == "*" {
        return true;
    }

    // Check for METHOD:/path patterns (HTTP routing)
    if let Some(idx) = pattern.find(":/") {
        let pattern_method = &pattern[..idx];
        let pattern_path = &pattern[idx + 1..];

        let msg_idx = match message_kind.find(":/") {
            Some(i) => i,
            None => return false,
        };
        let msg_method = &message_kind[..msg_idx];
        let msg_path = &message_kind[msg_idx + 1..];

        // Check method match ("*" matches any method)
        if pattern_method != "*" && pattern_method != msg_method {
            return false;
        }

        return match_path(pattern_path, msg_path);
    }

    if pattern == message_kind {
        return true;
    }

    if let Some(prefix) = pattern.strip_suffix(".**") {
        return message_kind.starts_with(&format!("{}.", prefix));
    }

    if let Some(prefix) = pattern.strip_suffix(".*") {
        if !message_kind.starts_with(&format!("{}.", prefix)) {
            return false;
        }
        let rest = &message_kind[prefix.len() + 1..];
        return !rest.contains('.');
    }

    false
}

/// Extract path variables from a matched pattern and set them as req.param.{name} meta.
pub fn extract_path_vars(pattern: &str, path: &str, msg: &mut Message) {
    // Strip /** suffix
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
                format!("{}{}", META_REQ_PARAM_PREFIX, var_name),
                path_parts[i].to_string(),
            );
        }
    }
}

/// matchPath checks if msgPath matches patternPath with ** wildcard and {var} support.
pub fn match_path(pattern: &str, path: &str) -> bool {
    if pattern == path {
        return true;
    }

    // ** suffix: match any sub-path
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path == prefix || path.starts_with(&format!("{}/", prefix));
    }

    // Segment-by-segment matching with {var} support
    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let path_parts: Vec<&str> = path.split('/').collect();

    if pattern_parts.len() != path_parts.len() {
        return false;
    }

    for (pp, actual) in pattern_parts.iter().zip(path_parts.iter()) {
        if pp.starts_with('{') && pp.ends_with('}') {
            continue;
        }
        if pp != actual {
            return false;
        }
    }

    true
}

/// Execute the first child node whose Match pattern matches msg.Kind.
pub(crate) fn execute_first_match_children(
    nodes: &[Box<Node>],
    msg: &mut Message,
) -> Option<(usize, bool)> {
    for (i, child) in nodes.iter().enumerate() {
        if !matches_pattern(&child.match_pattern, &msg.kind) {
            continue;
        }
        // Extract path variables from HTTP route patterns
        if !child.match_pattern.is_empty() {
            if let Some(idx) = child.match_pattern.find(":/") {
                let pattern_path = &child.match_pattern[idx + 1..];
                if let Some(msg_idx) = msg.kind.find(":/") {
                    let msg_path = msg.kind[msg_idx + 1..].to_string();
                    extract_path_vars(pattern_path, &msg_path, msg);
                }
            }
        }
        return Some((i, true));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_pattern_empty() {
        assert!(matches_pattern("", "anything"));
        assert!(matches_pattern("*", "anything"));
    }

    #[test]
    fn test_matches_pattern_exact() {
        assert!(matches_pattern("user.create", "user.create"));
        assert!(!matches_pattern("user.create", "user.delete"));
    }

    #[test]
    fn test_matches_pattern_single_wildcard() {
        assert!(matches_pattern("user.*", "user.create"));
        assert!(matches_pattern("user.*", "user.delete"));
        assert!(!matches_pattern("user.*", "user.create.done"));
    }

    #[test]
    fn test_matches_pattern_multi_wildcard() {
        assert!(matches_pattern("user.**", "user.create"));
        assert!(matches_pattern("user.**", "user.create.done"));
        assert!(!matches_pattern("user.**", "admin.create"));
    }

    #[test]
    fn test_matches_pattern_http() {
        assert!(matches_pattern("GET:/users", "GET:/users"));
        assert!(matches_pattern("*:/users", "GET:/users"));
        assert!(matches_pattern("*:/users", "POST:/users"));
        assert!(!matches_pattern("GET:/users", "POST:/users"));
        assert!(matches_pattern("GET:/users/**", "GET:/users/123"));
        assert!(matches_pattern("GET:/users/{id}", "GET:/users/123"));
    }

    #[test]
    fn test_match_path() {
        assert!(match_path("/auth/login", "/auth/login"));
        assert!(match_path("/auth/**", "/auth/login"));
        assert!(match_path("/auth/**", "/auth/login/callback"));
        assert!(match_path("/users/{id}", "/users/123"));
        assert!(!match_path("/users/{id}", "/users/123/edit"));
    }

    #[test]
    fn test_extract_path_vars() {
        let mut msg = Message::new("GET:/users/123", "");
        extract_path_vars("/users/{id}", "/users/123", &mut msg);
        assert_eq!(msg.var("id"), "123");
    }
}
