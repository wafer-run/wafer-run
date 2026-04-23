# LLM Service (wafer-run side) — Implementation Plan

**Goal:** Add `LlmService` trait + `MultiBackendLlmService` router + `wafer-run/llm` service block to wafer-core, mirroring the `DatabaseService`/`DatabaseBlock` pattern.

**Spec:** `../../../docs/superpowers/specs/2026-04-15-llm-service-refactor-design.md`

**Architecture:** The trait lives in `wafer-core/src/interfaces/llm/`. The service block lives in `wafer-core/src/service_blocks/llm.rs`. The block buffers the `ChatRequest` from `InputStream`, calls the service, streams `ChatChunk`s out as `StreamEvent::Chunk` (JSON-encoded) on `OutputStream`, terminates with `Complete` or `Error`. Cancellation on drop via `OutputStream`'s built-in `CancellationToken`.

**Sibling reference:** `crates/wafer-core/src/interfaces/database/` + `service_blocks/database.rs`.

---

## File Structure

```
crates/wafer-core/src/interfaces/llm/
├── mod.rs          # pub use service::*; pub use router::*; pub mod handler;
├── service.rs      # trait LlmService + all data types + LlmError
├── router.rs       # MultiBackendLlmService
└── handler.rs      # message dispatch (llm.chat, llm.list_models, etc.)

crates/wafer-core/src/service_blocks/
└── llm.rs          # LlmBlock + register_with

crates/wafer-core/src/interfaces/mod.rs   # add: pub mod llm;
crates/wafer-core/src/service_blocks/mod.rs  # add: pub mod llm;
```

Tests: `crates/wafer-core/tests/llm_service.rs` (integration-style, exercise router + block via a FakeLlmService).

---

## Tasks

### Task 1: Scaffold `interfaces/llm/` skeleton + wire into module tree

**Files:**
- Create: `crates/wafer-core/src/interfaces/llm/mod.rs`
- Create: `crates/wafer-core/src/interfaces/llm/service.rs` (empty stub with placeholder type)
- Create: `crates/wafer-core/src/interfaces/llm/router.rs` (empty stub)
- Create: `crates/wafer-core/src/interfaces/llm/handler.rs` (empty stub)
- Modify: `crates/wafer-core/src/interfaces/mod.rs` — add `pub mod llm;`

Goal: crate compiles with an empty module in place. Verify with `cargo check -p wafer-core`.

**Commit:** `feat(wafer-core): scaffold llm interface module`

---

### Task 2: Data types + `LlmError` in `service.rs`

**File:** `crates/wafer-core/src/interfaces/llm/service.rs`

Add all request/response/model types from spec §"The `LlmService` Trait › Types":
- Request: `ChatRequest`, `ChatParams`, `ResponseFormat`, `ChatMessage`, `ChatRole`, `ChatContent`, `ContentPart`, `ToolDefinition`, `ToolCall`
- Response: `ChatChunk`, `ChunkDelta`, `FinishReason`, `TokenUsage`
- Model mgmt: `ModelInfo`, `ModelCapabilities`, `ModelStatus`, `ModelState`, `LoadProgress`
- Error: `LlmError` (thiserror)

All structs `#[non_exhaustive]`. `Serialize`/`Deserialize` on all data types (required — handler JSON-encodes them). `Default` on `ChatParams` and `ModelCapabilities`.

**Test:** round-trip `ChatRequest` + `ChatChunk` through serde_json, assert equality on a few variants (text chunk, tool-call delta, usage chunk).

**Commit:** `feat(wafer-core): llm types — request, response, model, error`

---

### Task 3: `LlmService` trait

**File:** `crates/wafer-core/src/interfaces/llm/service.rs` (same file)

Add the trait per spec §"Trait definition". Use `tokio_util::sync::CancellationToken`. Bound: `MaybeSend + MaybeSync + 'static`. `async_trait` cfg_attr split for wasm vs native matching `DatabaseService`.

Method set:
- `async fn chat_stream(&self, req: ChatRequest, cancel: CancellationToken) -> BoxStream<'static, Result<ChatChunk, LlmError>>`
- `async fn list_models(&self) -> Result<Vec<ModelInfo>, LlmError>`
- `async fn status(&self, backend_id: &str, model_id: &str) -> Result<ModelStatus, LlmError>`
- `fn load_model(...) -> BoxStream<'static, Result<LoadProgress, LlmError>>` — default impl: `NotSupported`
- `async fn unload_model(...)` — default: `NotSupported`
- `fn claims_backend(&self, _: &str) -> bool` — default: `false`

Note the default impls exactly as in spec.

`BoxStream` = `futures::stream::BoxStream`.

**Verify:** `cargo check -p wafer-core` passes.

**Commit:** `feat(wafer-core): LlmService trait`

---

### Task 4: `MultiBackendLlmService` router

**File:** `crates/wafer-core/src/interfaces/llm/router.rs`

Copy router impl from spec §"The MultiBackendLlmService Router". Methods:
- `new()`, `register(label, service)` builder
- `find(backend_id)` private helper
- Full `LlmService` impl with dispatch + aggregation on `list_models`

`list_models` keeps going on per-backend errors (logs warning, continues) per spec.

**Tests (in same file, `#[cfg(test)]`):**
Create a `FakeLlmService` helper with builder:
- `returning_text(s: &str)` — streams one `ChunkDelta::Text(s)` on chat
- `with_models(Vec<ModelInfo>)` — returns on `list_models`
- `claims(backend_id)` — controls `claims_backend`

Then the 3 unit tests from spec §"Testing Strategy › Unit tests — the trait and the router":
1. `router_dispatches_by_backend_id`
2. `router_list_models_aggregates_across_backends`
3. `default_load_model_returns_not_supported`

**Commit:** `feat(wafer-core): MultiBackendLlmService router + tests`

---

### Task 5: Handler — message dispatch in `handler.rs`

**File:** `crates/wafer-core/src/interfaces/llm/handler.rs`

Decodes `msg.kind` and dispatches. Signature:

```rust
pub async fn handle_message(
    service: Arc<dyn LlmService>,
    _ctx: &dyn Context,
    msg: Message,
    input: InputStream,
) -> OutputStream
```

Unlike `database/handler.rs` (buffered-only), LLM needs streaming for `llm.chat` and `llm.load_model`. For streaming ops:

1. Build `OutputStream::new_streaming()` → `(sink, cancel)`
2. Decode request from `input.collect_to_bytes()` (body is bounded — JSON `ChatRequest`)
3. Spawn task (`tokio::spawn` on native, `spawn_local` on wasm — check existing wafer-core patterns) to drive the service stream into the sink
4. Pipe each `ChatChunk` as `StreamEvent::Chunk(serde_json::to_vec(chunk)?)`
5. Forward cancellation from `OutputStream`'s cancel token into the service's cancel token
6. Terminate with `Complete` on stream-end, `Error` on error
7. Return the `OutputStream`

For buffered ops (`llm.list_models`, `llm.status`, `llm.unload_model`): call service, JSON-encode result, return `OutputStream::from_bytes(...)` or equivalent (check wafer-block API).

Operation kinds:
- `"llm.chat"` → streaming `chat_stream`
- `"llm.list_models"` → buffered
- `"llm.status"` → buffered (read `backend_id`/`model_id` from body JSON)
- `"llm.load_model"` → streaming `load_model`
- `"llm.unload_model"` → buffered
- unknown kind → `OutputStream` terminating with `WaferError::InvalidRequest` or similar

**Commit:** `feat(wafer-core): llm handler with streaming chat dispatch`

---

### Task 6: `LlmBlock` service block + `register_with`

**File:** `crates/wafer-core/src/service_blocks/llm.rs`

Mirrors `database.rs` exactly. `handle` delegates to `handler::handle_message`. No `lifecycle` hook needed (providers are configured on the underlying service by the feature block, not at block lifecycle). Minimal `BlockInfo` — name `"wafer-run/llm"`, protocol e.g. `"llm@v1"`, category `Service`.

Also modify: `crates/wafer-core/src/service_blocks/mod.rs` — add `pub mod llm;`.

**Commit:** `feat(wafer-core): wafer-run/llm service block`

---

### Task 7: Integration test — block end-to-end via FakeLlmService

**File:** `crates/wafer-core/tests/llm_service.rs`

Register `LlmBlock::new(Arc::new(fake))` in a test runtime. Send a `Message { kind: "llm.chat", ... }` with a JSON `ChatRequest` in the body. Assert:
- Output stream yields expected `Chunk(...)` frames in order
- Terminates with `Complete`
- Dropping the output stream mid-flight cancels the service (verify via a Fake that records cancel)

Also a buffered test: `llm.list_models` returns aggregated JSON `Vec<ModelInfo>`.

**Commit:** `test(wafer-core): llm service block integration`

---

### Task 8: Public API surface — re-exports for consumers

**File:** `crates/wafer-core/src/lib.rs` (or wherever the public surface is)

Ensure consumers can do:
```rust
use wafer_core::interfaces::llm::{
    LlmService, MultiBackendLlmService, ChatRequest, ChatChunk, LlmError, /* ... */
};
use wafer_core::service_blocks::llm::register_with;
```

Check existing crates reexport pattern (database seems to already be accessible via `wafer_core::interfaces::database`).

**Verify:** compile a tiny downstream test binary if one exists, otherwise `cargo check --workspace`.

**Commit:** `feat(wafer-core): export LLM service public API`

---

### Task 9: Format + PR

```bash
cargo +nightly fmt --all
cargo test -p wafer-core
cargo clippy -p wafer-core --no-deps -- -D warnings
```

Push, open PR against `main`. Title: `feat(wafer-core): LlmService trait + wafer-run/llm service block`.

---

## Out of scope for this PR
- Provider impls (OpenAI/Anthropic) — in solobase.
- BrowserLlmService — in solobase-web.
- Feature block rewrite — in solobase.
- Migration of existing provider_llm DB data — in solobase.

## Self-review

**Spec coverage:** §Trait, §Router, §Service Block, §Testing Strategy (unit tests for trait/router) — all tasks present. §ProviderLlmService, §BrowserLlmService, §Feature block — correctly deferred to solobase PR.

**Type consistency:** `backend_id: String`, `model: String`, `model_id: String` (when separate), `ChatChunk.delta` → `ChunkDelta` enum — consistent across tasks.

**Placeholders:** none — each task names exact files + what goes there. Where the full code is long (types, router), it's by reference to the spec (same repo, `../solobase/docs/...`), not hand-waved.
