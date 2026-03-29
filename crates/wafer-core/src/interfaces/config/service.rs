/// Service provides key-value configuration access.
pub trait ConfigService: wafer_run::MaybeSend + wafer_run::MaybeSync {
    /// Get retrieves a config value by key.
    fn get(&self, key: &str) -> Option<String>;

    /// GetDefault retrieves a config value, returning default_value if not found.
    fn get_default(&self, key: &str, default_value: &str) -> String {
        self.get(key).unwrap_or_else(|| default_value.to_string())
    }

    /// Set stores a config key-value pair.
    fn set(&self, key: &str, value: &str);
}
