//! Ergonomic accessors for request metadata stored on [`Message`] under the
//! conventional `req.*` / `http.header.*` key prefixes from [`crate::meta`].

use crate::{meta::*, Message};

/// Extension trait for the `Message` type.
/// Provides ergonomic read-only accessors for request metadata.
pub trait MessageExt {
    /// Return the URL path variable `name` (from `req.param.{name}`), or `""`.
    fn var(&self, name: &str) -> &str;
    /// Return the query parameter `name` (from `req.query.{name}`), or `""`.
    fn query(&self, name: &str) -> &str;
    /// Return the HTTP request header `name` (case-insensitive), or `""`.
    fn header(&self, name: &str) -> &str;
}

impl MessageExt for Message {
    fn var(&self, name: &str) -> &str {
        let key = format!("{META_REQ_PARAM_PREFIX}{name}");
        self.get_meta(&key)
    }

    fn query(&self, name: &str) -> &str {
        let key = format!("{META_REQ_QUERY_PREFIX}{name}");
        self.get_meta(&key)
    }

    fn header(&self, name: &str) -> &str {
        let key = format!("http.header.{name}");
        let val = self.get_meta(&key);
        if !val.is_empty() {
            return val;
        }
        let key_lower = format!("http.header.{}", name.to_lowercase());
        self.get_meta(&key_lower)
    }
}
