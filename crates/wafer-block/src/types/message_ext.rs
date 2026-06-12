//! Ergonomic impls on the core types ([`crate::Message`],
//! [`crate::WaferError`], [`crate::InstanceMode`]) plus the meta-access
//! traits ([`MetaGet`] / [`MetaSet`]) and `HashMap` conversions.

use std::collections::HashMap;

impl crate::WaferError {
    /// Create a new error with the given code and message (empty meta).
    pub fn new(code: crate::ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            meta: Vec::new(),
        }
    }

    /// Attach a precise, machine-readable application error code as
    /// structured meta under [`crate::meta::META_ERROR_CODE`].
    ///
    /// The coarse [`crate::ErrorCode`] stays the transport-level
    /// classification; this carries the app-level code (e.g.
    /// `auth.invalid_token`) so callers don't prefix it into the
    /// human-readable message. Replaces any existing entry.
    #[must_use]
    pub fn with_detail_code(mut self, code: impl Into<String>) -> Self {
        MetaSet::set(
            &mut self.meta,
            crate::meta::META_ERROR_CODE.to_string(),
            code.into(),
        );
        self
    }

    /// Read the structured application error code set via
    /// [`Self::with_detail_code`], if any.
    pub fn detail_code(&self) -> Option<&str> {
        MetaGet::get(self.meta.as_slice(), crate::meta::META_ERROR_CODE)
    }
}

impl crate::Message {
    /// Create a new message with the given kind (empty meta).
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            meta: Vec::new(),
        }
    }

    /// Get a meta value by key, returning `""` if not found.
    pub fn get_meta(&self, key: &str) -> &str {
        self.meta
            .iter()
            .find(|entry| entry.key == key)
            .map_or("", |entry| entry.value.as_str())
    }

    /// Set a meta value, updating an existing entry or appending a new one.
    pub fn set_meta(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        if let Some(entry) = self.meta.iter_mut().find(|e| e.key == key) {
            entry.value = value;
        } else {
            self.meta.push(crate::MetaEntry { key, value });
        }
    }

    /// Shortcut for `get_meta(META_REQ_ACTION)`.
    pub fn action(&self) -> &str {
        self.get_meta(crate::meta::META_REQ_ACTION)
    }

    /// Shortcut for `get_meta(META_REQ_RESOURCE)`.
    pub fn path(&self) -> &str {
        self.get_meta(crate::meta::META_REQ_RESOURCE)
    }

    /// Shortcut for the authenticated user ID.
    pub fn user_id(&self) -> &str {
        self.get_meta(crate::meta::META_AUTH_USER_ID)
    }

    /// Shortcut for `get_meta("req.client.ip")`.
    pub fn remote_addr(&self) -> &str {
        self.get_meta(crate::meta::META_REQ_CLIENT_IP)
    }

    /// Get a cookie value by name from the Cookie header.
    pub fn cookie(&self, name: &str) -> &str {
        let cookies = self.header("cookie");
        if cookies.is_empty() {
            return "";
        }
        for part in cookies.split(';') {
            let part = part.trim();
            if let Some(eq) = part.find('=') {
                if &part[..eq] == name {
                    return &part[eq + 1..];
                }
            }
        }
        ""
    }

    /// Get an HTTP header value (case-insensitive).
    pub fn header(&self, name: &str) -> &str {
        let key = format!("http.header.{name}");
        let val = self.get_meta(&key);
        if !val.is_empty() {
            return val;
        }
        let key_lower = format!("http.header.{}", name.to_lowercase());
        self.get_meta(&key_lower)
    }

    /// Get a URL path variable by name (from `req.param.{name}`).
    pub fn var(&self, name: &str) -> &str {
        let key = format!("{}{}", crate::meta::META_REQ_PARAM_PREFIX, name);
        self.get_meta(&key)
    }

    /// Get a query parameter by name (from `req.query.{name}`).
    pub fn query(&self, name: &str) -> &str {
        let key = format!("{}{}", crate::meta::META_REQ_QUERY_PREFIX, name);
        self.get_meta(&key)
    }

    /// Collect all query parameters into a HashMap.
    pub fn query_params(&self) -> HashMap<String, String> {
        let prefix = crate::meta::META_REQ_QUERY_PREFIX;
        self.meta
            .iter()
            .filter(|e| e.key.starts_with(prefix))
            .map(|e| (e.key[prefix.len()..].to_string(), e.value.clone()))
            .collect()
    }

    /// Parse pagination query parameters (page, page_size, offset).
    ///
    /// `page` is an unbounded, externally-supplied query value, so the offset is
    /// computed with saturating arithmetic: a hostile `?page=<huge>` clamps the
    /// offset to `usize::MAX` (yielding an empty page) instead of overflowing —
    /// which would panic in debug builds and silently wrap in release.
    pub fn pagination_params(&self, default_page_size: usize) -> (usize, usize, usize) {
        let page: usize = self.query("page").parse().unwrap_or(1).max(1);
        let page_size: usize = self
            .query("page_size")
            .parse()
            .unwrap_or(default_page_size)
            .min(100);
        let offset = page.saturating_sub(1).saturating_mul(page_size);
        (page, page_size, offset)
    }
}

/// Read-only `HashMap`-like access to a sequence of [`crate::MetaEntry`].
///
/// Implemented for both `Vec<MetaEntry>` and the `[MetaEntry]` slice, so
/// callers holding only a `&[MetaEntry]` can still look entries up. Mutation
/// lives on the separate [`MetaSet`] trait, which is implemented only for the
/// owning `Vec` — a slice can't grow, so it can never expose a setter.
pub trait MetaGet {
    /// Look up the value for `key`, returning `None` if absent.
    fn get(&self, key: &str) -> Option<&str>;
    /// Whether an entry for `key` exists.
    fn contains_key(&self, key: &str) -> bool;
}

/// Mutating `HashMap`-like access to an owning `Vec<MetaEntry>`.
///
/// Deliberately *not* implemented for `[MetaEntry]`: a slice is a fixed-size
/// view and cannot accept new entries, so insertion is only meaningful on the
/// owning `Vec`. Splitting this off from [`MetaGet`] makes that a compile-time
/// guarantee rather than a runtime panic.
pub trait MetaSet {
    /// Set `key` to `value`, replacing any existing entry with the same key.
    fn set(&mut self, key: String, value: String);
}

impl MetaGet for Vec<crate::MetaEntry> {
    fn get(&self, key: &str) -> Option<&str> {
        // Delegate to the slice impl so the lookup logic lives in one place.
        MetaGet::get(self.as_slice(), key)
    }

    fn contains_key(&self, key: &str) -> bool {
        MetaGet::contains_key(self.as_slice(), key)
    }
}

impl MetaSet for Vec<crate::MetaEntry> {
    fn set(&mut self, key: String, value: String) {
        if let Some(entry) = self.iter_mut().find(|e| e.key == key) {
            entry.value = value;
        } else {
            self.push(crate::MetaEntry { key, value });
        }
    }
}

impl MetaGet for [crate::MetaEntry] {
    fn get(&self, key: &str) -> Option<&str> {
        self.iter().find(|e| e.key == key).map(|e| e.value.as_str())
    }

    fn contains_key(&self, key: &str) -> bool {
        self.iter().any(|e| e.key == key)
    }
}

/// Convert a `HashMap<String, String>` into `Vec<MetaEntry>`.
pub fn hashmap_to_meta(map: HashMap<String, String>) -> Vec<crate::MetaEntry> {
    map.into_iter()
        .map(|(k, v)| crate::MetaEntry { key: k, value: v })
        .collect()
}

/// Convert `Vec<MetaEntry>` into `HashMap<String, String>`.
pub fn meta_to_hashmap(meta: &[crate::MetaEntry]) -> HashMap<String, String> {
    meta.iter()
        .map(|e| (e.key.clone(), e.value.clone()))
        .collect()
}

impl std::fmt::Display for crate::InstanceMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            crate::InstanceMode::PerNode => write!(f, "per-node"),
            crate::InstanceMode::Singleton => write!(f, "singleton"),
            crate::InstanceMode::PerFlow => write!(f, "per-flow"),
            crate::InstanceMode::PerExecution => write!(f, "per-execution"),
        }
    }
}

impl crate::InstanceMode {
    /// Parse an instance mode from a string (e.g. from flow config).
    pub fn parse(s: &str) -> Self {
        match s {
            "singleton" => Self::Singleton,
            "per-flow" => Self::PerFlow,
            "per-execution" => Self::PerExecution,
            _ => Self::PerNode,
        }
    }
}

#[cfg(test)]
mod meta_access_tests {
    use super::*;
    use crate::MetaEntry;

    fn sample() -> Vec<MetaEntry> {
        vec![
            MetaEntry {
                key: "a".into(),
                value: "1".into(),
            },
            MetaEntry {
                key: "b".into(),
                value: "2".into(),
            },
        ]
    }

    #[test]
    fn slice_exposes_read_methods() {
        let v = sample();
        let slice: &[MetaEntry] = v.as_slice();
        assert_eq!(MetaGet::get(slice, "a"), Some("1"));
        assert_eq!(MetaGet::get(slice, "missing"), None);
        assert!(MetaGet::contains_key(slice, "b"));
        assert!(!MetaGet::contains_key(slice, "missing"));
    }

    #[test]
    fn vec_read_methods_match_slice() {
        let v = sample();
        assert_eq!(MetaGet::get(&v, "a"), Some("1"));
        assert!(MetaGet::contains_key(&v, "b"));
        assert!(!MetaGet::contains_key(&v, "missing"));
    }

    #[test]
    fn vec_set_inserts_then_replaces() {
        let mut v = sample();
        // Insert a new key.
        MetaSet::set(&mut v, "c".into(), "3".into());
        assert_eq!(MetaGet::get(&v, "c"), Some("3"));
        assert_eq!(v.len(), 3);
        // Replace an existing key in place (no growth).
        MetaSet::set(&mut v, "a".into(), "99".into());
        assert_eq!(MetaGet::get(&v, "a"), Some("99"));
        assert_eq!(v.len(), 3);
    }
}

#[cfg(test)]
mod wafer_error_detail_code_tests {
    use crate::{meta::META_ERROR_CODE, types::MetaGet, ErrorCode, WaferError};

    #[test]
    fn with_detail_code_sets_structured_meta() {
        let err = WaferError::new(ErrorCode::InvalidArgument, "bad email")
            .with_detail_code("auth.invalid_email");
        assert_eq!(err.detail_code(), Some("auth.invalid_email"));
        assert_eq!(
            MetaGet::get(err.meta.as_slice(), META_ERROR_CODE),
            Some("auth.invalid_email")
        );
        // The human-readable message stays human-only.
        assert_eq!(err.message, "bad email");
    }

    #[test]
    fn with_detail_code_replaces_existing_entry() {
        let err = WaferError::new(ErrorCode::Internal, "boom")
            .with_detail_code("a.first")
            .with_detail_code("b.second");
        assert_eq!(err.detail_code(), Some("b.second"));
        assert_eq!(err.meta.len(), 1);
    }

    #[test]
    fn detail_code_absent_is_none() {
        let err = WaferError::new(ErrorCode::NotFound, "nope");
        assert_eq!(err.detail_code(), None);
    }
}

#[cfg(test)]
mod pagination_tests {
    fn msg_with_query(name: &str, value: &str) -> crate::Message {
        let mut m = crate::Message::new("test");
        m.set_meta(
            format!("{}{}", crate::meta::META_REQ_QUERY_PREFIX, name),
            value,
        );
        m
    }

    #[test]
    fn defaults_to_page_one_when_absent() {
        let m = crate::Message::new("test");
        assert_eq!(m.pagination_params(25), (1, 25, 0));
    }

    #[test]
    fn computes_offset_from_page_and_size() {
        let m = msg_with_query("page", "3");
        assert_eq!(m.pagination_params(20), (3, 20, 40));
    }

    #[test]
    fn page_size_is_capped_at_100() {
        let mut m = msg_with_query("page", "1");
        m.set_meta(
            format!("{}page_size", crate::meta::META_REQ_QUERY_PREFIX),
            "10000",
        );
        let (_, page_size, _) = m.pagination_params(20);
        assert_eq!(page_size, 100);
    }

    #[test]
    fn huge_page_saturates_offset_instead_of_overflowing() {
        // Regression: an unbounded `?page=` query value must not overflow the
        // `(page - 1) * page_size` multiply (debug panic / release wraparound).
        let m = msg_with_query("page", &usize::MAX.to_string());
        let (page, page_size, offset) = m.pagination_params(50);
        assert_eq!(page, usize::MAX);
        assert_eq!(page_size, 50);
        assert_eq!(offset, usize::MAX);
    }
}
