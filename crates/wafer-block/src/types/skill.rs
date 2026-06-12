//! Skill / agent metadata — [`SkillRole`], [`SkillTool`], and
//! [`ExternalAsset`] (consumed by gizza-ai/agent and any future agent block).

/// Marker for blocks that should be enumerated as tools by an agent block.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillRole {
    /// Block is callable as a tool by an agent block.
    Skill,
}

/// JSON-Schema-shaped tool descriptor for OpenAI-compatible function calling.
/// Mirrors the shape consumed by WebLLM and remote LLM providers.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillTool {
    /// Natural-language description shown to the LLM.
    pub description: String,
    /// Free-form JSON Schema describing the tool's input arguments.
    pub parameters: serde_json::Value,
}

/// Declarative pointer to a heavy external WASM/JS asset that the host
/// loads lazily on first use (e.g. ffmpeg-core.wasm from a CDN).
///
/// `loader` is a controlled vocabulary on the host side. Known values:
/// - `"ffmpeg.wasm"` — initialised via `@ffmpeg/ffmpeg`'s `createFFmpeg`.
///
/// New loader values require a host update; new assets that target an
/// existing loader do not.
///
/// `timeout_ms` lets the block override the host's default load timeout
/// (currently 120s in solobase-browser's `bridge.js`). `None` keeps the
/// host default. Useful for assets whose CDN download legitimately takes
/// longer than the default on slow links (e.g. ffmpeg-core ~31 MB).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExternalAsset {
    /// Stable asset identifier used by the host loader (e.g. `"ffmpeg-core"`).
    pub id: String,
    /// Controlled-vocabulary loader name the host knows how to invoke.
    pub loader: String,
    /// Asset version string for cache-busting/audit.
    pub version: String,
    /// Source URL the host fetches the asset from.
    pub url: String,
    /// Expected SHA-256 (hex) of the asset bytes, verified after download.
    pub sha256: String,
    /// Optional per-asset load timeout in milliseconds. When `None`, the
    /// host applies its default. `skip_serializing_if = "Option::is_none"`
    /// keeps the JSON wire format byte-identical for callers that don't
    /// set the field, so existing serialized payloads remain unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u32>,
}
