//! Data model for a parsed WaferFlow document.
//!
//! These structs mirror the JSON schema published at
//! `site/public/schema/waferflow/` and are produced by [`crate::parse`].

use std::{collections::HashMap, fmt, num::NonZeroU64, str::FromStr, time::Duration};

use serde::{Deserialize, Serialize};

use crate::error::InvalidTimeout;

/// A complete WaferFlow definition: metadata, typed input/output ports,
/// the ordered list of [`Step`]s to execute, and optional flow-level config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaferFlow {
    /// Stable identifier for the flow (used by the runtime registry).
    pub id: String,
    /// Human-readable name displayed in admin UIs and logs.
    pub name: String,
    /// SemVer string for the flow definition itself (not the runtime).
    pub version: String,
    /// Free-form description shown in introspection endpoints.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema describing the data the caller must supply as `$.input`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<PortSchema>,
    /// JSON Schema describing the value returned to the caller.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<PortSchema>,
    /// Ordered list of steps; execution starts at `steps[0]` unless a
    /// `next` entry redirects.
    pub steps: Vec<Step>,
    /// Optional flow-level execution policy (timeouts, step budget, error handling).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<FlowConfig>,
    /// Block dependencies — the runtime ensures these blocks are registered
    /// before the flow is allowed to execute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<String>>,
    /// Declarative config routing: maps user-facing config keys to
    /// `(target block, key within that block's config)` pairs so a flow
    /// can expose a flat config surface that fans out to its component blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_map: Option<HashMap<String, ConfigMapEntry>>,
    /// Static config values always injected into target block configs at
    /// resolve time (in addition to anything routed via `config_map`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_defaults: Option<HashMap<String, serde_json::Value>>,
}

/// A single step in a flow — one block invocation plus its input template
/// and (optionally) the routing rules for the next step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Step identifier unique within the flow; also the key under which
    /// the step's output is stored in the [`crate::Accumulator`].
    pub id: String,
    /// Registered block name to invoke (e.g. `"crypto/jwt-sign"`).
    pub block: String,
    /// JSON template passed to the block as input. String values starting
    /// with `$.` are resolved against the accumulator at execution time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    /// Conditional routing entries evaluated in order; the first whose
    /// `when` is true (or the lone default entry) wins.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<Vec<NextEntry>>,
    /// `$.path` to an array whose elements drive a per-item fan-out.
    /// The inner step iteration sees the current item as `$.each.item`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub each: Option<String>,
    /// Parallel branches run concurrently; the step completes when all
    /// branches finish.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel: Option<Vec<ParallelBranch>>,
    /// Human-readable description shown in introspection endpoints.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Per-step block config merged into `RuntimeContext.config` for this
    /// invocation only (overrides flow- and instance-level config).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

/// One branch of a [`Step::parallel`] fan-out. Each branch is its own
/// ordered list of steps that executes concurrently with its siblings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelBranch {
    /// Ordered steps run within this branch.
    pub steps: Vec<Step>,
}

/// A single routing rule inside [`Step::next`]: when `when` evaluates to
/// true, jump to `step` (within this flow) or `flow` (a different flow id).
/// An entry without `when` is the default fallthrough.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextEntry {
    /// Optional `when` expression. Omit for the default fallthrough entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Target step id within the current flow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// Target flow id to hand control over to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow: Option<String>,
}

/// JSON Schema subset used to describe typed flow / step ports.
///
/// Mirrors a handful of JSON Schema keywords (`type`, `properties`, `items`,
/// `required`, `default`, `description`) — enough for the introspection UI
/// without pulling in a full schema library.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortSchema {
    /// JSON Schema `type` keyword (`"object"`, `"array"`, `"string"`, ...).
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub schema_type: Option<String>,
    /// For `object` schemas, the property-name → sub-schema map.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<std::collections::HashMap<String, PortSchema>>,
    /// For `array` schemas, the element schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<PortSchema>>,
    /// For `object` schemas, the list of required property names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
    /// Free-form description shown in introspection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Default value supplied when the caller omits this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}

/// Flow-level execution policy: timeouts and budgets that wrap the whole
/// step sequence rather than any single step.
///
/// Every field is typed: an unknown key, an `on_error` other than `"stop"` /
/// `"continue"`, a zero `max_steps`, or a `timeout` / `timeout_ms` that is
/// zero, malformed or above [`MAX_FLOW_TIMEOUT`] fails [`crate::parse`]. Setting both
/// `timeout` and `timeout_ms` fails [`crate::validate`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowConfig {
    /// Hard timeout for the entire flow, in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<FlowTimeoutMillis>,
    /// Human-readable timeout (e.g. `"30s"`) — alternative to `timeout_ms`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<FlowTimeout>,
    /// Cap on the number of step executions, to prevent infinite loops.
    /// Defaults to 1000.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<NonZeroU64>,
    /// How the runtime reacts when a step errors. Defaults to [`OnError::Stop`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_error: Option<OnError>,
}

impl FlowConfig {
    /// The flow's timeout, from whichever of `timeout` / `timeout_ms` is set
    /// ([`crate::validate`] rejects a config that sets both). `None` means
    /// the flow has no timeout. Never zero and never above
    /// [`MAX_FLOW_TIMEOUT`].
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
            .as_ref()
            .map(FlowTimeout::duration)
            .or_else(|| self.timeout_ms.map(FlowTimeoutMillis::duration))
    }

    /// The effective error policy: `on_error`, or [`OnError::Stop`] when unset.
    pub fn on_error(&self) -> OnError {
        self.on_error.unwrap_or_default()
    }
}

/// What the runtime does when a step returns an error.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnError {
    /// Stop the flow and return the step's error.
    #[default]
    Stop,
    /// Record the failed step's output as `null` and run the next step.
    Continue,
}

/// The longest timeout a flow may set: 24 hours. A flow is one request's
/// work, so anything longer is a typo (a unit slip, extra digits); refusing
/// it at load also keeps every deadline far inside what the clock can
/// represent.
pub const MAX_FLOW_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// Check a parsed timeout against the allowed range `(0, MAX_FLOW_TIMEOUT]`.
fn check_timeout_range(duration: Duration) -> Result<Duration, &'static str> {
    if duration.is_zero() {
        Err("a flow timeout must be greater than zero")
    } else if duration > MAX_FLOW_TIMEOUT {
        Err("a flow timeout must be at most 24h")
    } else {
        Ok(duration)
    }
}

/// A flow timeout written as `"<n>ms"`, `"<n>s"`, `"<n>m"`, `"<n>h"`, or a
/// bare `"<n>"` (seconds), where `<n>` is a decimal integer.
///
/// Parsing rejects anything else, zero, and anything above
/// [`MAX_FLOW_TIMEOUT`]. The text is kept so a flow serializes back to what
/// its author wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct FlowTimeout {
    text: String,
    duration: Duration,
}

impl FlowTimeout {
    /// The parsed duration (never zero).
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// The timeout as written.
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl FromStr for FlowTimeout {
    type Err = InvalidTimeout;

    fn from_str(text: &str) -> Result<Self, InvalidTimeout> {
        let invalid = |reason: &'static str| InvalidTimeout {
            value: text.to_string(),
            reason,
        };
        let (digits, unit_secs, is_millis) = if let Some(n) = text.strip_suffix("ms") {
            (n, 1, true)
        } else if let Some(n) = text.strip_suffix('s') {
            (n, 1, false)
        } else if let Some(n) = text.strip_suffix('m') {
            (n, 60, false)
        } else if let Some(n) = text.strip_suffix('h') {
            (n, 3600, false)
        } else {
            (text, 1, false)
        };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid(
                "expected a whole number followed by ms, s, m or h (e.g. \"30s\")",
            ));
        }
        // A number too wide for u64, or one whose seconds overflow it, is
        // far above the maximum.
        let too_long = || invalid("a flow timeout must be at most 24h");
        let n: u64 = digits.parse().map_err(|_| too_long())?;
        let duration = if is_millis {
            Duration::from_millis(n)
        } else {
            Duration::from_secs(n.checked_mul(unit_secs).ok_or_else(too_long)?)
        };
        Ok(Self {
            text: text.to_string(),
            duration: check_timeout_range(duration).map_err(invalid)?,
        })
    }
}

impl TryFrom<String> for FlowTimeout {
    type Error = InvalidTimeout;

    fn try_from(text: String) -> Result<Self, InvalidTimeout> {
        text.parse()
    }
}

impl From<FlowTimeout> for String {
    fn from(timeout: FlowTimeout) -> String {
        timeout.text
    }
}

/// A flow's `timeout_ms`: whole milliseconds in `1..=86_400_000` (up to
/// [`MAX_FLOW_TIMEOUT`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct FlowTimeoutMillis(u64);

impl FlowTimeoutMillis {
    /// The timeout (never zero, never above [`MAX_FLOW_TIMEOUT`]).
    pub fn duration(self) -> Duration {
        Duration::from_millis(self.0)
    }
}

impl TryFrom<u64> for FlowTimeoutMillis {
    type Error = InvalidTimeout;

    fn try_from(millis: u64) -> Result<Self, InvalidTimeout> {
        check_timeout_range(Duration::from_millis(millis))
            .map(|_| Self(millis))
            .map_err(|reason| InvalidTimeout {
                value: format!("{millis}ms"),
                reason,
            })
    }
}

impl From<FlowTimeoutMillis> for u64 {
    fn from(timeout: FlowTimeoutMillis) -> u64 {
        timeout.0
    }
}

impl fmt::Display for FlowTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// One row of [`WaferFlow::config_map`]: a user-facing flow-config key is
/// forwarded into `target` block's config under `key`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigMapEntry {
    /// Block name receiving the value (e.g. `"crypto/jwt-sign"`).
    pub target: String,
    /// Key within the target block's config to populate.
    pub key: String,
}

/// Lightweight flow descriptor returned by introspection endpoints —
/// enough to list flows in admin UIs without serializing the full body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowInfo {
    /// Stable flow identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Free-form description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn timeout_units() {
        for (text, expected) in [
            ("250ms", Duration::from_millis(250)),
            ("30s", Duration::from_secs(30)),
            ("5m", Duration::from_secs(300)),
            ("2h", Duration::from_secs(7200)),
            ("45", Duration::from_secs(45)),
        ] {
            let timeout: FlowTimeout = text.parse().unwrap();
            assert_eq!(timeout.duration(), expected, "{text}");
            assert_eq!(timeout.as_str(), text);
        }
    }

    #[test]
    fn timeout_above_24h_is_an_error() {
        // The first four are just past 24h; 5124095576030428h fits in u64
        // seconds (~585 billion years); the last three overflow u64 seconds
        // or u64 itself.
        for text in [
            "25h",
            "86401s",
            "1441m",
            "86400001ms",
            "5124095576030428h",
            "5124095576030432h",
            "18446744073709551615m",
            "18446744073709551616s",
        ] {
            let err = text.parse::<FlowTimeout>().unwrap_err();
            assert_eq!(err.reason, "a flow timeout must be at most 24h", "{text}");
        }
        for text in ["24h", "1440m", "86400s", "86400000ms"] {
            let timeout: FlowTimeout = text.parse().unwrap();
            assert_eq!(timeout.duration(), MAX_FLOW_TIMEOUT, "{text}");
        }
    }

    #[test]
    fn timeout_ms_range() {
        assert_eq!(
            FlowTimeoutMillis::try_from(86_400_000).unwrap().duration(),
            MAX_FLOW_TIMEOUT
        );
        for bad in [0, 86_400_001, u64::MAX] {
            assert!(FlowTimeoutMillis::try_from(bad).is_err(), "{bad}");
        }
    }
}
