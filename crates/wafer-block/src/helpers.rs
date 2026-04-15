use crate::meta::*;
use crate::Message;

/// Extension trait for the `Message` type.
/// Provides ergonomic read-only accessors for request metadata.
pub trait MessageExt {
    fn var(&self, name: &str) -> &str;
    fn query(&self, name: &str) -> &str;
    fn header(&self, name: &str) -> &str;
}

impl MessageExt for Message {
    fn var(&self, name: &str) -> &str {
        let key = format!("{}{}", META_REQ_PARAM_PREFIX, name);
        self.get_meta(&key)
    }

    fn query(&self, name: &str) -> &str {
        let key = format!("{}{}", META_REQ_QUERY_PREFIX, name);
        self.get_meta(&key)
    }

    fn header(&self, name: &str) -> &str {
        let key = format!("http.header.{}", name);
        let val = self.get_meta(&key);
        if !val.is_empty() {
            return val;
        }
        let key_lower = format!("http.header.{}", name.to_lowercase());
        self.get_meta(&key_lower)
    }
}
