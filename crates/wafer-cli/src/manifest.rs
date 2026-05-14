use std::path::Path;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

/// The manifest.json format for a WAFER block project.
///
/// Fields like `capabilities`, `wasm_size`, and `sha256` are optional and
/// are added by `wafer package` at build/publish time.
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    /// Block name in `{org}/{block}` format — exactly one "/" required.
    pub name: String,
    pub version: String,
    pub interface: String,
    pub summary: String,
    #[serde(default)]
    pub requires: Vec<String>,
    // Build-time enrichment — added by `wafer package`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wasm_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

impl Manifest {
    /// Read and validate the manifest.json in `dir`.
    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        let path = dir.join("manifest.json");
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let manifest: Manifest = serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate that the name is in `{org}/{block}` format (exactly one "/").
    fn validate(&self) -> anyhow::Result<()> {
        let parts: Vec<&str> = self.name.splitn(3, '/').collect();
        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            bail!(
                "Invalid block name {:?}: must be in {{org}}/{{block}} format (exactly one \"/\")",
                self.name
            );
        }
        Ok(())
    }

    /// Write a default manifest.json template into `dir`.
    pub fn write_template(dir: &Path, name: &str) -> anyhow::Result<()> {
        let manifest = Manifest {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            interface: "handler@v1".to_string(),
            summary: format!("A WAFER block: {name}"),
            requires: vec![],
            capabilities: None,
            wasm_size: None,
            sha256: None,
        };
        let json =
            serde_json::to_string_pretty(&manifest).context("Failed to serialize manifest")?;
        let path = dir.join("manifest.json");
        std::fs::write(&path, json + "\n")
            .with_context(|| format!("Failed to write {}", path.display()))?;
        Ok(())
    }
}
