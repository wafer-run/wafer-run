//! Structured error envelope parsing for registry server responses.
//!
//! The site (`wafer-run/site`) returns `{"error": "<kebab>", "message": "<str>"}`
//! on every non-2xx response. This module parses that envelope when present
//! and renders a one-line error with an optional hint. Callers go through
//! [`crate::registry_client::ensure_ok`] which constructs [`RegistryError`].

use std::fmt;

#[derive(Debug, serde::Deserialize)]
pub struct ErrorEnvelope {
    #[expect(dead_code, reason = "read by hint() which is wired in a later task")]
    pub error: String,
    pub message: String,
}

#[derive(Debug)]
pub struct RegistryError {
    pub op: &'static str,
    pub status: reqwest::StatusCode,
    pub envelope: Option<ErrorEnvelope>,
    pub raw_body: String,
}

impl RegistryError {
    /// Build from a reqwest response body. Truncates the raw body to
    /// 512 chars (on a char boundary) for safe display.
    pub fn new(op: &'static str, status: reqwest::StatusCode, body: String) -> Self {
        let envelope = serde_json::from_str::<ErrorEnvelope>(&body).ok();
        let raw_body = truncate_chars(body, 512);
        Self {
            op,
            status,
            envelope,
            raw_body,
        }
    }

    fn hint(&self) -> Option<&'static str> {
        None // filled in by a later task
    }
}

fn truncate_chars(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        return s;
    }
    s.chars().take(max).collect()
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.envelope {
            Some(env) => write!(f, "{} failed ({}): {}", self.op, self.status, env.message)?,
            None if self.raw_body.is_empty() => write!(f, "{} failed ({})", self.op, self.status)?,
            None => write!(f, "{} failed ({}): {}", self.op, self.status, self.raw_body)?,
        }
        if let Some(h) = self.hint() {
            write!(f, "\nhint: {h}")?;
        }
        Ok(())
    }
}

impl std::error::Error for RegistryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn parses_envelope_and_renders_message() {
        let err = RegistryError::new(
            "publish",
            StatusCode::CONFLICT,
            r#"{"error":"version-exists","message":"acme/widget@0.1.0 already published"}"#.into(),
        );
        let s = err.to_string();
        assert!(s.contains("publish failed (409 Conflict)"), "{s}");
        assert!(s.contains("acme/widget@0.1.0 already published"), "{s}");
        // Raw JSON must not leak into the rendered message.
        assert!(!s.contains("\"error\""), "raw JSON leaked: {s}");
    }

    #[test]
    fn falls_back_to_raw_body_when_not_envelope() {
        let err = RegistryError::new(
            "publish",
            StatusCode::INTERNAL_SERVER_ERROR,
            "<html>500 Internal Server Error</html>".into(),
        );
        let s = err.to_string();
        assert!(
            s.contains("publish failed (500 Internal Server Error)"),
            "{s}"
        );
        assert!(s.contains("<html>"), "{s}");
    }

    #[test]
    fn empty_body_has_no_trailing_colon() {
        let err = RegistryError::new("publish", StatusCode::BAD_GATEWAY, String::new());
        assert_eq!(err.to_string(), "publish failed (502 Bad Gateway)");
    }

    #[test]
    fn truncates_long_body_to_512_chars() {
        let body = "x".repeat(10_000);
        let err = RegistryError::new("publish", StatusCode::INTERNAL_SERVER_ERROR, body);
        // Truncated body portion; "publish failed (500 Internal Server Error): " prefix is ~44 chars.
        assert!(
            err.raw_body.chars().count() <= 512,
            "len={}",
            err.raw_body.chars().count()
        );
    }
}
