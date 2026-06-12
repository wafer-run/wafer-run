//! Remote-block machinery (`wasm` feature): parsing `{org}/{block}@{version}`
//! references, fetching registry manifests, and downloading `.wasm` /
//! `.flow.json` artifacts during [`Wafer::seal`].

use std::{collections::HashMap, sync::Arc};

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

impl Wafer {
    /// Build the short-lived HTTP client used for registry/manifest fetches
    /// during one `seal()` pass.
    pub(crate) fn registry_http_client() -> Result<reqwest::Client, RuntimeError> {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| RuntimeError::Registry(format!("failed to create HTTP client: {e}")))
    }

    /// Resolve remote blocks for deferred registrations via the registry.
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

        for name in candidates {
            let Some(remote_ref) =
                parse_versioned_block(&name).or_else(|| parse_unversioned_block(&name))
            else {
                continue;
            };

            let Some(entry) = fetch_manifest_entry(&client, &remote_ref, &name).await? else {
                // Not in the registry (404) — skip this candidate.
                continue;
            };

            if let Some(flow_url) = &entry.flow_url {
                let flow = self
                    .download_flow_from_url(&client, flow_url, &name)
                    .await?;

                // Pre-resolve block dependencies from the flow's blocks list
                let blocks_to_resolve: Vec<String> = flow
                    .blocks
                    .as_ref()
                    .map(|b| {
                        b.iter()
                            .filter(|b| !self.registration.blocks.contains_key(b.as_str()))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();

                self.add_flow(flow);

                for block_name in &blocks_to_resolve {
                    if self.registration.blocks.contains_key(block_name.as_str()) {
                        continue;
                    }
                    match self.resolve_remote_block(&client, block_name).await {
                        Ok(Some(block)) => {
                            tracing::info!(block = %block_name, "downloaded remote block");
                            self.registration.register_remote_block(block_name, block)?;
                        }
                        Ok(None) => {
                            tracing::debug!(
                                block = %block_name,
                                "block not found in registry, will resolve during step resolution"
                            );
                        }
                        Err(e) => {
                            return Err(RuntimeError::Registry(format!(
                                "failed to download block dependency {block_name:?} for flow {name:?}: {e}"
                            )));
                        }
                    }
                }
            } else if let Some(wasm_url) = &entry.wasm_url {
                let block = self
                    .download_wasm_from_url(&client, wasm_url, &name)
                    .await?;
                tracing::info!(block = %name, "downloaded remote WASM block from registry");
                self.registration.register_remote_block(&name, block)?;
            }
        }

        Ok(())
    }

    /// Download a `.flow.json` from a direct URL and parse as WaferFlow.
    async fn download_flow_from_url(
        &self,
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

        let body = resp
            .bytes()
            .await
            .map_err(|e| RuntimeError::Flow(format!("failed to read flow body for {name}: {e}")))?;

        let body_str = std::str::from_utf8(&body).map_err(|e| {
            RuntimeError::Flow(format!("failed to decode flow body for {name}: {e}"))
        })?;

        let flow = wafer_flow::parse(body_str).map_err(|e| {
            RuntimeError::Flow(format!("failed to parse flow JSON for {name}: {e}"))
        })?;

        tracing::info!(flow = %flow.id, url = %url, "downloaded remote flow definition");
        Ok(flow)
    }

    /// Download a `.wasm` block from a direct URL.
    async fn download_wasm_from_url(
        &mut self,
        client: &reqwest::Client,
        url: &str,
        name: &str,
    ) -> Result<Arc<dyn Block>, RuntimeError> {
        use crate::wasm::{capabilities::BlockCapabilities, WasmiBlock};

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

        let body = resp
            .bytes()
            .await
            .map_err(|e| RuntimeError::Wasm(format!("failed to read WASM body for {name}: {e}")))?;

        if body.is_empty() {
            return Err(RuntimeError::Wasm(format!(
                "failed to download WASM for {name}: empty response body"
            )));
        }

        let engine = self.wasm_engine()?.clone();
        let block = WasmiBlock::load_with_engine(&engine, &body, BlockCapabilities::none())
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
        let Some(remote_ref) =
            parse_versioned_block(name).or_else(|| parse_unversioned_block(name))
        else {
            return Ok(None);
        };

        let Some(entry) = fetch_manifest_entry(client, &remote_ref, name).await? else {
            return Ok(None);
        };

        if let Some(wasm_url) = &entry.wasm_url {
            let block = self.download_wasm_from_url(client, wasm_url, name).await?;
            Ok(Some(block))
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

    let manifest_bytes = resp
        .bytes()
        .await
        .map_err(|e| RuntimeError::Registry(format!("failed to read manifest for {name}: {e}")))?;
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
