//! Flow-level execution policy helpers.
//!
//! `FlowConfig` lives in the dependency-free `wafer-flow` leaf crate, so
//! timeout resolution (which needs `parse_duration` from `wafer-block`) is
//! expressed here as an extension trait rather than an inherent method.

use std::time::Duration;

use wafer_flow::FlowConfig;

use crate::config::parse_duration;

/// Resolve flow-level execution policy from a [`FlowConfig`].
pub(crate) trait FlowConfigExt {
    /// Resolve the effective flow timeout: prefer the human-readable
    /// `timeout` string, fall back to `timeout_ms`. Returns `None` if unset,
    /// empty/invalid, or zero.
    fn resolve_timeout(&self) -> Option<Duration>;
}

impl FlowConfigExt for FlowConfig {
    fn resolve_timeout(&self) -> Option<Duration> {
        if let Some(ref t) = self.timeout {
            let d = parse_duration(t);
            if !d.is_zero() {
                return Some(d);
            }
        }
        self.timeout_ms
            .map(Duration::from_millis)
            .filter(|d| !d.is_zero())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(timeout: Option<&str>, timeout_ms: Option<u64>) -> FlowConfig {
        FlowConfig {
            timeout: timeout.map(str::to_string),
            timeout_ms,
            max_steps: None,
            on_error: None,
        }
    }

    #[test]
    fn string_timeout_wins_over_ms() {
        let c = cfg(Some("30s"), Some(5000));
        assert_eq!(c.resolve_timeout(), Some(Duration::from_secs(30)));
    }

    #[test]
    fn falls_back_to_timeout_ms() {
        let c = cfg(None, Some(5000));
        assert_eq!(c.resolve_timeout(), Some(Duration::from_millis(5000)));
    }

    #[test]
    fn invalid_string_falls_back_to_ms() {
        let c = cfg(Some("not-a-duration"), Some(2000));
        assert_eq!(c.resolve_timeout(), Some(Duration::from_millis(2000)));
    }

    #[test]
    fn empty_string_falls_back_to_ms() {
        let c = cfg(Some(""), Some(2000));
        assert_eq!(c.resolve_timeout(), Some(Duration::from_millis(2000)));
    }

    #[test]
    fn zero_string_falls_back_to_ms() {
        let c = cfg(Some("0s"), Some(2000));
        assert_eq!(c.resolve_timeout(), Some(Duration::from_millis(2000)));
    }

    #[test]
    fn zero_timeout_ms_is_none() {
        let c = cfg(None, Some(0));
        assert_eq!(c.resolve_timeout(), None);
    }

    #[test]
    fn both_unset_is_none() {
        let c = cfg(None, None);
        assert_eq!(c.resolve_timeout(), None);
    }
}
