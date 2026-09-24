/// Service provides key-value configuration access.
pub trait ConfigService: wafer_block::MaybeSend + wafer_block::MaybeSync {
    /// The value of `key`, or `None` when it is not set.
    fn get(&self, key: &str) -> Option<String>;

    /// The value of `key`, or `default_value` when it is not set.
    fn get_default(&self, key: &str, default_value: &str) -> String {
        self.get(key).unwrap_or_else(|| default_value.to_string())
    }

    /// Set stores a config key-value pair.
    fn set(&self, key: &str, value: &str);
}
