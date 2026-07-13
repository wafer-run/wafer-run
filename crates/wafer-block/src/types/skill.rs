//! Skill / agent metadata — [`SkillTool`] and [`ExternalAsset`]
//! (consumed by gizza-ai/agent and any future agent block).

/// JSON-Schema-shaped tool descriptor for OpenAI-compatible function calling.
/// Mirrors the shape consumed by WebLLM and remote LLM providers.
///
/// A block is an agent-callable skill iff it carries a `SkillTool` (there is no
/// separate marker — the old single-variant `SkillRole` enum was always set in
/// lock-step with this field and carried no extra information, so it was
/// removed). `BlockInfo::tool.is_some()` is the sole predicate.
///
/// Deferred design — an `invocable_by` gate. Not built yet because no
/// autonomous-LLM tool-calling path exists today (every tool is invoked only
/// via explicit user slash-commands), so it would re-create the always-set /
/// never-read flag just deleted. Build it the day autonomous invocation lands;
/// it is a pure additive serde change (no migration). Intended shape:
///   `enum Invoker { Manual, Autonomous }`  (mode axis: "is a human in the loop?")
///   `invocable_by: BTreeSet<Invoker>` on `SkillTool`, default `{Manual}`.
/// `{}` nobody · `{Manual}` user-only (slash) · `{Autonomous}` agent-only · both = full.
/// Rationale: mode axis not actor (planner/cron/workflow are also autonomous);
/// default `{Manual}` already neutralizes model-self-invocation of a destructive
/// tool (`{All}` silently widens on a new invoker, `None`-default is inert);
/// a typed set not `tags: Vec<String>` (an authz gate must be exhaustive +
/// compiler-checked, not magic strings); `Cli`/`Api` do NOT belong here —
/// those are transport, already governed by endpoints + auth levels.
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
/// (currently 120s in the browser build's `bridge.js`). `None` keeps the
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
