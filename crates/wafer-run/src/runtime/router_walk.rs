//! Collects router-route block references for `seal()`-time validation.
//!
//! Walks every block config whose canonical name resolves to
//! `wafer-run/router` (including aliases), parses the `routes` JSON
//! array, and emits one `(canonical_block_name, BlockReferenceSource::RouterRoute)`
//! tuple per route entry for the seal-time aggregator in
//! [`super::resolver`].
//!
//! Duplicates the route-shape parser from `wafer-block-router::parse_routes`
//! intentionally: `wafer-run` cannot depend on the leaf router crate
//! (Wave 4 layering). The shape is contract-pinned by the
//! `seal_router_route_contract_match_with_block_parser` integration test
//! in `tests/seal_router_route_resolution.rs`.

use super::Wafer;
use crate::error::BlockReferenceSource;

const ROUTER_BLOCK: &str = "wafer-run/router";

/// Walks every `wafer-run/router` config (canonical or aliased) and
/// emits one `(canonical_block_name, RouterRoute source)` tuple per
/// route entry. The `seal()` aggregator merges these into its
/// references map alongside flow-step references.
pub(super) fn collect_router_route_refs(wafer: &Wafer) -> Vec<(String, BlockReferenceSource)> {
    // 1. Identify every config key pointing at a wafer-run/router instance:
    //    the canonical name itself, plus any alias whose direct target is
    //    `wafer-run/router`. Wave 17 PR A makes chained aliases statically
    //    impossible (rejected at `add_alias` time), so a single-hop check
    //    suffices.
    let mut router_keys: Vec<String> = vec![ROUTER_BLOCK.to_string()];
    for (alias, target) in wafer.aliases.iter() {
        if target == ROUTER_BLOCK {
            router_keys.push(alias.clone());
        }
    }

    // 2. For each router-config key present in block_configs, parse
    //    routes and emit references.
    let mut refs: Vec<(String, BlockReferenceSource)> = Vec::new();
    for key in router_keys {
        let Some(config) = wafer.block_configs.get(&key) else {
            continue;
        };
        for route in parse_routes_for_validation_with_key(config, &key) {
            let canonical = wafer.canonicalize(&route.block).to_string();
            refs.push((
                canonical,
                BlockReferenceSource::RouterRoute {
                    router_block: key.clone(),
                    path: route.path,
                    actions: route.raw_actions,
                },
            ));
        }
    }

    refs
}

/// One parsed route entry, retained for diagnostics.
///
/// `pub` so the contract-pinning integration test in
/// `tests/seal_router_route_resolution.rs` can read the fields without an
/// accessor layer.
pub struct ValidationRoute {
    /// `path` field from the route entry.
    pub path: String,
    /// Raw action/method strings as the operator wrote them. We
    /// deliberately do NOT normalize here — the router crate normalizes
    /// for matching; for diagnostics we want the operator's original
    /// strings so the error message matches their config.
    pub raw_actions: Vec<String>,
    /// `block` field from the route entry (pre-alias-resolution).
    pub block: String,
}

/// Shape-pinned to `wafer-block-router::parse_routes`. Same field
/// extraction (`path`, `block`, `actions`/`methods`); only difference
/// is that we keep `raw_actions` un-normalized.
///
/// `pub` so the contract test in `tests/seal_router_route_resolution.rs`
/// can compare against the router crate's parser on identical input.
///
/// Malformed entries (missing `path`/`block` or non-string values) are
/// silently dropped, matching the router's runtime parser. For operator
/// diagnostics, prefer [`parse_routes_for_validation_with_key`] which
/// emits a `tracing::warn!` per dropped entry tagged with the router
/// config key.
pub fn parse_routes_for_validation(config: &serde_json::Value) -> Vec<ValidationRoute> {
    parse_routes_inner(config)
        .into_iter()
        .filter_map(Result::ok)
        .collect()
}

/// Same as [`parse_routes_for_validation`] but emits a `tracing::warn!`
/// for every dropped malformed entry, tagged with the router config
/// key (canonical or alias) so operators can locate the bad entry in
/// the config they wrote. seal() still accepts the rejection (matching
/// the runtime parser's behavior — we don't fail seal() on a malformed
/// route entry), so the only effect of this path is observability.
fn parse_routes_for_validation_with_key(
    config: &serde_json::Value,
    router_block: &str,
) -> Vec<ValidationRoute> {
    parse_routes_inner(config)
        .into_iter()
        .filter_map(|entry| match entry {
            Ok(route) => Some(route),
            Err((reason, raw)) => {
                tracing::warn!(
                    router_block = %router_block,
                    reason = %reason,
                    entry = %raw,
                    "skipped malformed route entry during seal validation"
                );
                None
            }
        })
        .collect()
}

/// Per-entry parse result. `Err` carries `(reason, raw_entry_json)` so
/// the caller can log a useful warning. Splitting this out keeps the
/// shape contract test (which uses [`parse_routes_for_validation`])
/// agnostic of the warning channel.
fn parse_routes_inner(
    config: &serde_json::Value,
) -> Vec<Result<ValidationRoute, (&'static str, String)>> {
    let Some(arr) = config.get("routes").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .map(|entry| {
            let Some(path_val) = entry.get("path") else {
                return Err(("missing `path` field", entry.to_string()));
            };
            let Some(path) = path_val.as_str() else {
                return Err(("`path` is not a string", entry.to_string()));
            };
            let Some(block_val) = entry.get("block") else {
                return Err(("missing `block` field", entry.to_string()));
            };
            let Some(block) = block_val.as_str() else {
                return Err(("`block` is not a string", entry.to_string()));
            };
            let raw = entry
                .get("actions")
                .or_else(|| entry.get("methods"))
                .and_then(|m| m.as_array());
            let raw_actions = raw
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            Ok(ValidationRoute {
                path: path.to_string(),
                raw_actions,
                block: block.to_string(),
            })
        })
        .collect()
}
