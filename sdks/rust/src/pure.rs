//! Pure block SDK for WaferFlow.
//!
//! Pure blocks are fully sandboxed WASM components — they receive JSON input
//! and return JSON output with no host imports. Use this module to build
//! blocks for WaferFlow steps.
//!
//! # Example
//!
//! ```rust,ignore
//! use wafer_sdk::pure::PureBlock;
//! use wafer_sdk::pure::BlockDef;
//!
//! struct UpperCase;
//!
//! impl PureBlock for UpperCase {
//!     fn handle(&self, input: &[u8]) -> Result<Vec<u8>, String> {
//!         let val: serde_json::Value = serde_json::from_slice(input)
//!             .map_err(|e| e.to_string())?;
//!         let text = val.get("text")
//!             .and_then(|v| v.as_str())
//!             .ok_or("missing 'text' field")?;
//!         let result = serde_json::json!({ "result": text.to_uppercase() });
//!         serde_json::to_vec(&result).map_err(|e| e.to_string())
//!     }
//!
//!     fn info(&self) -> BlockDef {
//!         BlockDef {
//!             id: "@my-org/my-repo/uppercase".into(),
//!             name: "UpperCase".into(),
//!             version: "0.1.0".into(),
//!             description: Some("Converts text to uppercase".into()),
//!             input: None,
//!             output: None,
//!             runtime: Some("wasm".into()),
//!         }
//!     }
//! }
//! ```

/// Trait for implementing pure blocks.
///
/// Pure blocks are deterministic — same input always produces the same output.
/// They cannot make host calls, access the filesystem, or perform I/O.
pub trait PureBlock {
    /// Process JSON input bytes and return JSON output bytes.
    fn handle(&self, input: &[u8]) -> Result<Vec<u8>, String>;

    /// Return metadata describing this block.
    fn info(&self) -> BlockDef;
}

/// Block definition metadata for pure blocks.
#[derive(Debug, Clone)]
pub struct BlockDef {
    /// Fully-qualified block id, e.g. `@my-org/my-repo/uppercase`.
    pub id: String,
    /// Human-readable block name shown in tooling.
    pub name: String,
    /// Semver version string for this block.
    pub version: String,
    /// Optional short description of what the block does.
    pub description: Option<String>,
    /// Optional JSON schema describing the expected input shape.
    pub input: Option<String>,
    /// Optional JSON schema describing the produced output shape.
    pub output: Option<String>,
    /// Runtime identifier (e.g. `"wasm"`) advertised to WaferFlow.
    pub runtime: Option<String>,
}

impl BlockDef {
    /// Serialize to JSON string for the WIT `info()` export.
    pub fn to_json(&self) -> String {
        let mut obj = serde_json::Map::new();
        obj.insert("id".into(), serde_json::Value::String(self.id.clone()));
        obj.insert("name".into(), serde_json::Value::String(self.name.clone()));
        obj.insert(
            "version".into(),
            serde_json::Value::String(self.version.clone()),
        );
        if let Some(desc) = &self.description {
            obj.insert(
                "description".into(),
                serde_json::Value::String(desc.clone()),
            );
        }
        if let Some(runtime) = &self.runtime {
            obj.insert("runtime".into(), serde_json::Value::String(runtime.clone()));
        }
        serde_json::Value::Object(obj).to_string()
    }
}
