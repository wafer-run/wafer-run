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

/// Returns true if `name` resolves (transitively, through aliases) to
/// [`ROUTER_BLOCK`]. Bounded to 32 hops so a cyclic alias graph still
/// terminates with `false` instead of looping.
fn resolves_to_router(wafer: &Wafer, name: &str) -> bool {
    if name == ROUTER_BLOCK {
        return true;
    }
    let mut current = name;
    for _ in 0..32 {
        let Some(next) = wafer.aliases.get(current) else {
            return false;
        };
        if next == ROUTER_BLOCK {
            return true;
        }
        current = next.as_str();
    }
    false // alias chain too deep or cyclic
}

/// Walks every `wafer-run/router` config (canonical or aliased) and
/// emits one `(canonical_block_name, RouterRoute source)` tuple per
/// route entry. The `seal()` aggregator merges these into its
/// references map alongside flow-step references.
pub(super) fn collect_router_route_refs(wafer: &Wafer) -> Vec<(String, BlockReferenceSource)> {
    // 1. Identify every config key pointing at a wafer-run/router instance:
    //    the canonical name itself, plus any alias that transitively resolves
    //    to it (alias-of-alias chains). Cycle-guarded.
    let mut router_keys: Vec<String> = vec![ROUTER_BLOCK.to_string()];
    for alias in wafer.aliases.keys() {
        if alias == ROUTER_BLOCK {
            continue; // canonical already pushed
        }
        if resolves_to_router(wafer, alias) {
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
        for route in parse_routes_for_validation(config) {
            let canonical = wafer
                .aliases
                .get(&route.block)
                .cloned()
                .unwrap_or(route.block.clone());
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
pub fn parse_routes_for_validation(config: &serde_json::Value) -> Vec<ValidationRoute> {
    config
        .get("routes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| {
                    let path = entry.get("path")?.as_str()?.to_string();
                    let block = entry.get("block")?.as_str()?.to_string();
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
                    Some(ValidationRoute {
                        path,
                        raw_actions,
                        block,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}
