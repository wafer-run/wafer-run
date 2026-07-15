//! Remote-block machinery (`wasm` feature): parsing `{org}/{block}@{version}`
//! references, fetching registry manifests, and downloading `.wasm` /
//! `.flow.json` artifacts during [`Wafer::seal`].

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use futures::{StreamExt, TryStreamExt};
use wafer_block::{error::RuntimeError, Block};

use super::Wafer;

/// ABI version for WASM block compatibility.
pub const ABI_VERSION: u32 = 1;

/// Base URL for raw registry manifest fetches
/// (`{base}/{org}/{block}/manifest.json`).
const REGISTRY_MANIFEST_BASE_URL: &str =
    "https://raw.githubusercontent.com/wafer-run/registry/main";

/// A parsed reference to a remote block, e.g. `"wafer-run/sqlite@0.3.0"`.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteBlockRef {
    /// Org slug (left-hand side of `org/block`).
    pub org: String,
    /// Block name (right-hand side of `org/block`).
    pub block: String,
    /// Semver-style version following `@`.
    pub version: String,
}

/// Parse a block name into a versioned `RemoteBlockRef` if it matches the
/// `{org}/{block}@{version}` convention.
///
/// Returns `None` for local block names (no `/`, no version,
/// wrong number of segments, or empty version).
pub fn parse_versioned_block(name: &str) -> Option<RemoteBlockRef> {
    let at_pos = name.rfind('@')?;
    let path = &name[..at_pos];
    let version = &name[at_pos + 1..];
    if version.is_empty() || version == "latest" {
        return None;
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() != 2 || segments.iter().any(|s| s.is_empty()) {
        return None;
    }
    Some(RemoteBlockRef {
        org: segments[0].to_string(),
        block: segments[1].to_string(),
        version: version.to_string(),
    })
}

/// Parse a block name into an unversioned `RemoteBlockRef` if it matches the
/// `{org}/{block}` convention. No `@version` suffix.
///
/// Returns `None` when the name has a version, no `/`, or wrong
/// number of segments.
pub fn parse_unversioned_block(name: &str) -> Option<RemoteBlockRef> {
    // Strip optional @latest suffix
    let name = name.strip_suffix("@latest").unwrap_or(name);
    if name.contains('@') {
        return None;
    }
    let segments: Vec<&str> = name.split('/').collect();
    if segments.len() != 2 || segments.iter().any(|s| s.is_empty()) {
        return None;
    }
    Some(RemoteBlockRef {
        org: segments[0].to_string(),
        block: segments[1].to_string(),
        version: "latest".to_string(),
    })
}

/// Registry manifest format for resolving remote blocks.
#[derive(serde::Deserialize)]
pub(crate) struct RegistryManifest {
    #[expect(
        dead_code,
        reason = "deserialized for round-trip fidelity; cross-checked elsewhere"
    )]
    pub(crate) name: String,
    pub(crate) latest: String,
    pub(crate) versions: HashMap<String, VersionEntry>,
}

/// A single version entry in a registry manifest.
#[derive(serde::Deserialize)]
pub(crate) struct VersionEntry {
    pub(crate) abi: u32,
    pub(crate) wasm_url: Option<String>,
    pub(crate) flow_url: Option<String>,
}

/// SEC-09: per-response download caps for registry fetches. Bound the memory a
/// single (possibly compromised) registry response can consume during `seal()`.
const MAX_MANIFEST_BYTES: usize = 256 * 1024;
const MAX_FLOW_BYTES: usize = 4 * 1024 * 1024;
const MAX_WASM_BYTES: usize = 64 * 1024 * 1024;

/// PERF-04: bounded fan-out for registry manifest/artifact fetches during
/// `seal()`. Small enough to stay polite to the registry origin, large
/// enough to overlap network latency across independent candidates.
const REMOTE_FETCH_CONCURRENCY: usize = 8;

/// Read a response body into memory, refusing more than `max` bytes. The
/// response must advertise a `Content-Length` (registry origins — GitHub raw,
/// CDNs — always do); a missing length means an unbounded chunked stream, which
/// is refused outright rather than buffered without limit. The advertised
/// length is rejected if it exceeds `max`, and the buffered body is re-checked
/// as defense-in-depth. Replaces the previous unbounded `.bytes()`.
async fn read_body_capped(
    resp: reqwest::Response,
    max: usize,
    what: &str,
) -> Result<Vec<u8>, RuntimeError> {
    let len = resp.content_length().ok_or_else(|| {
        RuntimeError::Registry(format!(
            "{what}: response has no Content-Length; refusing unbounded download"
        ))
    })?;
    if len > max as u64 {
        return Err(RuntimeError::Registry(format!(
            "{what} exceeds the {max}-byte limit (Content-Length {len})"
        )));
    }
    let body = resp
        .bytes()
        .await
        .map_err(|e| RuntimeError::Registry(format!("reading {what}: {e}")))?;
    if body.len() > max {
        return Err(RuntimeError::Registry(format!(
            "{what} exceeds the {max}-byte limit"
        )));
    }
    Ok(body.to_vec())
}

impl Wafer {
    /// Build the short-lived HTTP client used for registry/manifest fetches
    /// during one `seal()` pass.
    pub(crate) fn registry_http_client() -> Result<reqwest::Client, RuntimeError> {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            // SEC-09: do not follow redirects. Manifest-controlled `wasm_url` /
            // `flow_url` values are otherwise free to bounce the fetch through a
            // redirect chain to an unintended (e.g. internal) destination. A
            // registry that needs a redirect must publish the final URL.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| RuntimeError::Registry(format!("failed to create HTTP client: {e}")))
    }

    /// Resolve remote blocks for deferred registrations via the registry.
    ///
    /// PERF-04: all network work (manifest fetches + artifact downloads)
    /// runs with bounded concurrency over immutable data (`&client` only);
    /// runtime mutations (`add_flow`, wasm instantiation +
    /// `register_remote_block`) are applied sequentially after the joins.
    /// `buffered` (rather than `buffer_unordered`) keeps result order equal
    /// to candidate order, so registration stays deterministic for a given
    /// candidate list.
    pub(crate) async fn resolve_remote_entries(&mut self) -> Result<(), RuntimeError> {
        let candidates: Vec<String> = self
            .registration
            .block_configs
            .keys()
            .filter(|name| name.contains('/'))
            .filter(|name| !self.flows.contains_key(name.as_str()))
            .filter(|name| !self.registration.blocks.contains_key(name.as_str()))
            .filter(|name| {
                parse_unversioned_block(name).is_some() || parse_versioned_block(name).is_some()
            })
            .cloned()
            .collect();

        if candidates.is_empty() {
            return Ok(());
        }

        let client = Self::registry_http_client()?;

        // Phase 1 — network: fetch each candidate's manifest and artifact
        // (flow JSON or wasm bytes) concurrently.
        let fetched: Vec<(String, FetchedCandidate)> =
            futures::stream::iter(candidates.into_iter().map(|name| {
                let client = &client;
                async move {
                    let outcome = fetch_candidate(client, &name).await?;
                    Ok::<_, RuntimeError>((name, outcome))
                }
            }))
            .buffered(REMOTE_FETCH_CONCURRENCY)
            .try_collect()
            .await?;

        // Phase 2 — apply: register flows/blocks sequentially and collect
        // the flow block-dependencies that still need resolving. Dedup so a
        // dependency shared by several flows is fetched once; remember the
        // first flow that wanted it for error context.
        let mut deps: Vec<(String, String)> = Vec::new();
        let mut dep_seen: HashSet<String> = HashSet::new();
        for (name, outcome) in fetched {
            match outcome {
                FetchedCandidate::Skipped => {}
                FetchedCandidate::Flow(flow) => {
                    if let Some(blocks) = flow.blocks.as_ref() {
                        for block_name in blocks {
                            if !self.registration.blocks.contains_key(block_name.as_str())
                                && dep_seen.insert(block_name.clone())
                            {
                                deps.push((block_name.clone(), name.clone()));
                            }
                        }
                    }
                    self.add_flow(*flow);
                }
                FetchedCandidate::Wasm(bytes) => {
                    let block = self.load_wasm_block(&bytes, &name)?;
                    tracing::info!(block = %name, "downloaded remote WASM block from registry");
                    self.registration.register_remote_block(&name, block)?;
                }
            }
        }

        // A dependency may itself have been a candidate registered above.
        deps.retain(|(block_name, _)| !self.registration.blocks.contains_key(block_name.as_str()));
        if deps.is_empty() {
            return Ok(());
        }

        // Phase 3 — network: fetch dependency manifests + wasm bytes with
        // the same bounded fan-out.
        let fetched_deps: Vec<(String, String, Option<Vec<u8>>)> =
            futures::stream::iter(deps.into_iter().map(|(block_name, flow_name)| {
                let client = &client;
                async move {
                    match fetch_dependency_wasm(client, &block_name).await {
                        Ok(bytes) => Ok((block_name, flow_name, bytes)),
                        Err(e) => Err(RuntimeError::Registry(format!(
                            "failed to download block dependency {block_name:?} for flow {flow_name:?}: {e}"
                        ))),
                    }
                }
            }))
            .buffered(REMOTE_FETCH_CONCURRENCY)
            .try_collect()
            .await?;

        // Phase 4 — apply: instantiate + register sequentially.
        for (block_name, flow_name, bytes) in fetched_deps {
            match bytes {
                Some(bytes) => {
                    let block = self.load_wasm_block(&bytes, &block_name).map_err(|e| {
                        RuntimeError::Registry(format!(
                            "failed to download block dependency {block_name:?} for flow {flow_name:?}: {e}"
                        ))
                    })?;
                    tracing::info!(block = %block_name, "downloaded remote block");
                    self.registration
                        .register_remote_block(&block_name, block)?;
                }
                None => {
                    tracing::debug!(
                        block = %block_name,
                        "block not found in registry, will resolve during step resolution"
                    );
                }
            }
        }

        Ok(())
    }

    /// Instantiate downloaded `.wasm` bytes as a block against the shared
    /// engine.
    ///
    /// The shared engine's `consume_fuel` flag and the per-call limits
    /// passed here are both derived from the runtime's configuration, so a
    /// remote block honours the builder's `fuel_per_call` /
    /// `max_wasm_memory_pages` selection.
    fn load_wasm_block(
        &mut self,
        bytes: &[u8],
        name: &str,
    ) -> Result<Arc<dyn Block>, RuntimeError> {
        use crate::wasm::{capabilities::BlockCapabilities, WasmiBlock};

        let limits = self.wasm.resource_limits();
        let engine = self.wasm_engine()?.clone();
        let block = WasmiBlock::load_with_engine_and_limits(
            &engine,
            bytes,
            BlockCapabilities::none(),
            limits,
        )
        .map_err(|e| RuntimeError::Wasm(format!("failed to load remote block {name}: {e}")))?;

        Ok(Arc::new(block))
    }

    /// Resolve a remote block via the registry. Returns `Ok(None)` if the block
    /// is not found in the registry.
    pub(crate) async fn resolve_remote_block(
        &mut self,
        client: &reqwest::Client,
        name: &str,
    ) -> Result<Option<Arc<dyn Block>>, RuntimeError> {
        match fetch_dependency_wasm(client, name).await? {
            Some(bytes) => Ok(Some(self.load_wasm_block(&bytes, name)?)),
            None => Ok(None),
        }
    }
}

/// Network-fetched resolution outcome for one candidate name. Produced
/// concurrently in `resolve_remote_entries` phase 1; applied to the runtime
/// sequentially in phase 2.
enum FetchedCandidate {
    /// Not a registry-shaped name, not in the registry (404), or a manifest
    /// entry with no artifact URLs — skipped.
    Skipped,
    /// Manifest pointed at a flow: downloaded and parsed. Boxed — the
    /// parsed flow is far larger than the other variants.
    Flow(Box<wafer_flow::WaferFlow>),
    /// Manifest pointed at a wasm artifact: raw bytes, instantiated later.
    Wasm(Vec<u8>),
}

/// Fetch one candidate's manifest entry and its artifact. Pure network —
/// takes no runtime state, so candidates can be fetched concurrently.
async fn fetch_candidate(
    client: &reqwest::Client,
    name: &str,
) -> Result<FetchedCandidate, RuntimeError> {
    let Some(remote_ref) = parse_versioned_block(name).or_else(|| parse_unversioned_block(name))
    else {
        return Ok(FetchedCandidate::Skipped);
    };

    let Some(entry) = fetch_manifest_entry(client, &remote_ref, name).await? else {
        // Not in the registry (404) — skip this candidate.
        return Ok(FetchedCandidate::Skipped);
    };

    if let Some(flow_url) = &entry.flow_url {
        let flow = download_flow_from_url(client, flow_url, name).await?;
        Ok(FetchedCandidate::Flow(Box::new(flow)))
    } else if let Some(wasm_url) = &entry.wasm_url {
        let bytes = download_wasm_bytes(client, wasm_url, name).await?;
        Ok(FetchedCandidate::Wasm(bytes))
    } else {
        Ok(FetchedCandidate::Skipped)
    }
}

/// Fetch the `.wasm` bytes for a block via the registry. Pure network —
/// no runtime state — so dependency fetches can run concurrently;
/// instantiation happens afterwards via `Wafer::load_wasm_block`.
///
/// Returns `Ok(None)` when the name isn't registry-shaped, the registry has
/// no manifest for it, or the manifest entry is a flow rather than a WASM
/// block — callers defer those to step resolution.
async fn fetch_dependency_wasm(
    client: &reqwest::Client,
    name: &str,
) -> Result<Option<Vec<u8>>, RuntimeError> {
    let Some(remote_ref) = parse_versioned_block(name).or_else(|| parse_unversioned_block(name))
    else {
        return Ok(None);
    };

    let Some(entry) = fetch_manifest_entry(client, &remote_ref, name).await? else {
        return Ok(None);
    };

    if let Some(wasm_url) = &entry.wasm_url {
        Ok(Some(download_wasm_bytes(client, wasm_url, name).await?))
    } else if let Some(flow_url) = &entry.flow_url {
        tracing::debug!(block = %name, flow_url = %flow_url, "block is a flow, not a WASM block");
        Ok(None)
    } else {
        let crate_name = format!("wafer-block-{}", remote_ref.block);
        Err(RuntimeError::Registry(format!(
            "Block \"{name}\" is native-only and must be compiled in.\n\
             Add it with: cargo add {crate_name}"
        )))
    }
}

/// Download a `.flow.json` from a direct URL and parse as WaferFlow.
async fn download_flow_from_url(
    client: &reqwest::Client,
    url: &str,
    name: &str,
) -> Result<wafer_flow::WaferFlow, RuntimeError> {
    let resp = client
        .get(url)
        .header("User-Agent", "wafer-run/0.1.0")
        .send()
        .await
        .map_err(|e| RuntimeError::Flow(format!("failed to download flow for {name}: {e}")))?;

    if resp.status().as_u16() != 200 {
        return Err(RuntimeError::Flow(format!(
            "failed to download flow for {}: HTTP {}",
            name,
            resp.status().as_u16()
        )));
    }

    let body = read_body_capped(resp, MAX_FLOW_BYTES, &format!("flow for {name}")).await?;

    let body_str = std::str::from_utf8(&body)
        .map_err(|e| RuntimeError::Flow(format!("failed to decode flow body for {name}: {e}")))?;

    let flow = wafer_flow::parse(body_str)
        .map_err(|e| RuntimeError::Flow(format!("failed to parse flow JSON for {name}: {e}")))?;

    tracing::info!(flow = %flow.id, url = %url, "downloaded remote flow definition");
    Ok(flow)
}

/// Download a `.wasm` artifact from a direct URL, returning the raw bytes.
async fn download_wasm_bytes(
    client: &reqwest::Client,
    url: &str,
    name: &str,
) -> Result<Vec<u8>, RuntimeError> {
    let resp = client
        .get(url)
        .header("User-Agent", "wafer-run/0.1.0")
        .send()
        .await
        .map_err(|e| RuntimeError::Wasm(format!("failed to download WASM for {name}: {e}")))?;

    let status = resp.status().as_u16();
    if status != 200 {
        return Err(RuntimeError::Wasm(format!(
            "failed to download WASM for {name}: HTTP {status}"
        )));
    }

    let body = read_body_capped(resp, MAX_WASM_BYTES, &format!("WASM for {name}")).await?;

    if body.is_empty() {
        return Err(RuntimeError::Wasm(format!(
            "failed to download WASM for {name}: empty response body"
        )));
    }

    Ok(body)
}

/// Fetch the registry manifest for `remote_ref`, select the requested version
/// (resolving `"latest"` through the manifest's `latest` field), and check ABI
/// compatibility.
///
/// Returns `Ok(None)` when the registry has no manifest for the block (HTTP
/// 404) so callers decide how to proceed (skip the candidate / report
/// not-found). Any other non-200 status, parse failure, unknown version, or
/// ABI mismatch is an error.
///
/// Shared by [`Wafer::resolve_remote_entries`] and
/// [`Wafer::resolve_remote_block`], which keep only their flow/wasm download
/// branching.
async fn fetch_manifest_entry(
    client: &reqwest::Client,
    remote_ref: &RemoteBlockRef,
    name: &str,
) -> Result<Option<VersionEntry>, RuntimeError> {
    let manifest_url = format!(
        "{REGISTRY_MANIFEST_BASE_URL}/{}/{}/manifest.json",
        remote_ref.org, remote_ref.block
    );

    let resp = client
        .get(&manifest_url)
        .header("User-Agent", "wafer-run/0.1.0")
        .send()
        .await
        .map_err(|e| {
            RuntimeError::Registry(format!("failed to fetch registry manifest for {name}: {e}"))
        })?;

    if resp.status().as_u16() == 404 {
        return Ok(None);
    }
    if resp.status().as_u16() != 200 {
        return Err(RuntimeError::Registry(format!(
            "failed to fetch registry manifest for {}: HTTP {}",
            name,
            resp.status().as_u16()
        )));
    }

    let manifest_bytes =
        read_body_capped(resp, MAX_MANIFEST_BYTES, &format!("manifest for {name}")).await?;
    let mut manifest: RegistryManifest = serde_json::from_slice(&manifest_bytes).map_err(|e| {
        RuntimeError::Registry(format!("failed to parse registry manifest for {name}: {e}"))
    })?;

    let version = if remote_ref.version == "latest" {
        manifest.latest.clone()
    } else {
        remote_ref.version.clone()
    };

    let entry = manifest.versions.remove(&version).ok_or_else(|| {
        RuntimeError::Registry(format!(
            "version {version} not found in registry for {name}"
        ))
    })?;

    if entry.abi != ABI_VERSION {
        return Err(RuntimeError::AbiMismatch {
            name: name.to_string(),
            required: entry.abi,
            supported: ABI_VERSION,
        });
    }

    Ok(Some(entry))
}
