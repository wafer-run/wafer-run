# Runtime correctness: interface and config validation

**Date:** 2026-04-17
**Status:** Proposed
**Scope:** Spec 1 of 3 in the wafer-run hardening initiative

## Context

A recent architectural review identified two runtime correctness gaps:

1. **Block calls are stringly-typed.** `ctx.call_block("wafer-run/sqlite", msg)` has no interface validation. `BlockInfo.interface` is declared and stored in `interface_specs_snapshot` at runtime startup (`context.rs:39`, `runtime.rs:166`) but never consulted during dispatch. A flow misrouting a message to the wrong block type fails deep in the block's handler with a confusing error.
2. **Config presence is not enforced.** `BlockInfo::config_keys` lists required configuration, and `runtime.rs:414-425` validates the *naming prefix* of declared keys, but nothing checks that required keys are actually provided. Missing values silently default to empty strings; blocks must defensively code.

WRAP enforcement (earlier concern in the review) is already live — `context.rs:113-138` calls `wafer_block::wrap::check_access()` before every `call_block` when `META_WRAP_RESOURCE` is set, with 15 unit tests in `wafer-block/src/wrap.rs:232-683`. That work is done. This spec addresses the two remaining correctness gaps.

## Goals

- Reject `call_block` dispatches whose message action is not part of the target block's declared interface.
- Reject `Wafer::start()` when any registered block is missing required config keys.
- Ship both fixes without changing public types (`BlockInfo`, `ConfigVar`) or breaking existing examples and downstream consumers (solobase).

## Non-goals

- Strengthening WRAP further (already enforced).
- Caller-side dependency declarations (`BlockInfo::dependencies`) — rejected as too intrusive for the value.
- Flow-level interface schema validation — considered and deferred; the per-dispatch check already catches flow misrouting.
- Adding a `required: bool` field to `ConfigVar` — convention-based (empty default + no `auto_generate`) is sufficient.
- JS SDK, CLI `dev`, docs — covered in Specs 2 and 3.

## Design

### Module layout

One new file: `crates/wafer-run/src/runtime/validation.rs`. It exposes two functions and a small state type for warn-once tracking. Two integration points in existing files.

### Interface validation at dispatch

Added to `RuntimeContext::call_block()` in `crates/wafer-run/src/context.rs`, positioned **after** the existing WRAP check (`:113-138`) **and after** the target block has been resolved by name (so its `BlockInfo` is in hand), **before** `block.handle()` is invoked.

Algorithm:

1. Read the resolved target block's `BlockInfo.interface` string.
2. Look up matching `InterfaceSpec` in `ctx.interface_specs_snapshot`.
3. If found: the interface's `ActionSpec` list names every action it supports. Check that `msg.action()` appears.
   - Match → proceed to existing dispatch.
   - No match → return a single-event `OutputStream` carrying `StreamEvent::Error(WaferError::invalid_argument("block {target} with interface {interface} does not expose action {action}"))`. Dispatch does not occur.
4. If no matching `InterfaceSpec` (custom/unknown interface): warn once per block and proceed. This preserves backward compatibility for blocks declaring custom interfaces outside `wafer_block::interfaces::all()`.

**Warn-once state** lives on the `Wafer` struct as `warned_unknown_interfaces: Arc<Mutex<HashSet<String>>>` and is cloned into each `RuntimeContext` alongside the existing `interface_specs_snapshot`. Process-local; not deduplicated across restarts.

### Config presence validation at Init

Invoked from `Wafer::start()` in `crates/wafer-run/src/runtime/lifecycle.rs`, **before** any block lifecycle event is dispatched.

Algorithm:

1. For every registered block, fetch its resolved config map (built during registration).
2. For each `ConfigVar` in `info.config_keys`:
   - Skip if `!default.is_empty()` — the key has a default.
   - Skip if `auto_generate == true` — runtime will populate the value.
   - Otherwise, the key is required. Check the config map for a non-empty value.
   - If missing, record `(block_name, key)`.
3. After iterating *all* blocks: if any misses were recorded, `start()` returns `Err` with a single aggregated message listing every offending `(block, key)`. Lifecycle events are not dispatched.

Aggregation matters. On a fresh deployment with ten misconfigured blocks, the operator sees the full list in one shot instead of fix-restart-fix-restart.

### Error handling

- **Interface mismatch** → `WaferError::invalid_argument(...)` wrapped in `StreamEvent::Error`, surfaced through the streaming `OutputStream`. No new error variant.
- **Missing required config** → aggregated error on `Wafer::start()`'s existing `Result`. Format: `"startup failed: missing required config: [{block-name-1}: {KEY_A}, {KEY_B}; {block-name-2}: {KEY_C}]"`.
- **Unknown interface at dispatch** → `WARN`-level log line via `tracing`, once per block, no user-facing error. Allows custom interfaces to keep working while signaling to the block author that validation isn't catching mistakes for them.

### Edge cases

- **Block with no `config_keys`** → validator is a no-op for that block.
- **WASM blocks** → identical code path. Their `BlockInfo` is loaded via the `__wafer_info` export during registration; both validators see them the same as native blocks.
- **Ordering of dispatch checks** → WRAP first (existing, untouched), then interface-action check. If both would fail, the caller sees the WRAP error.
- **Flows** → no special handling. The flow executor already calls `ctx.call_block`, so the interface check runs for every flow step automatically.
- **`start()` retry** → if `start()` fails with missing config, the runtime is in a clean pre-start state. The operator fixes config and calls `start()` again.

## Pre-landing audit

Any existing block declaring a `ConfigVar` with `default: ""` and no `auto_generate` will, after this change lands, cause `start()` to fail unless the value is provided. A pre-landing audit across all 20+ built-in blocks is required and produces two deliverables:

1. **An inventory** listing every `ConfigVar` in the workspace whose `default.is_empty() && !auto_generate`, annotated with a verdict: *genuinely required* or *silently optional*.
2. **Fix commits** for each silently-optional entry: set an explicit non-empty default, or set `auto_generate = true` where the value is a secret, or reclassify as required and document the requirement in the block's rustdoc.

The audit is a discrete step in the plan and must precede wiring `validate_config_presence` into `Wafer::start()`. A parallel sub-audit surveys every `call_block(_, msg)` in the workspace, reading `msg.action()` values, and cross-references them against the target block's declared interface in `wafer_block::interfaces::all()`. Any mismatch is either a bug (fix the call site) or a gap in the interface spec (add the action); both must be resolved before wiring `validate_action_interface` into `call_block`.

## Testing

### Unit tests in `runtime/validation.rs`

- `config_required_empty_no_default_missing` — key with empty default, no value → collected as missing.
- `config_required_with_default_present` — key with non-empty default → passes.
- `config_auto_generate_skipped` — `auto_generate: true`, no value → passes.
- `config_multiple_missing_aggregated` — two missing keys across two blocks → error lists all four.
- `interface_valid_action` — known interface, message action in action list → passes.
- `interface_unknown_action` — action not in interface's action list → returns `InvalidArgument` with block/interface/action in the message.
- `interface_unknown_interface_warns_once` — block declares `"my-org/custom@v1"` (not in `interfaces::all()`); two consecutive calls produce exactly one warning log line.

### Integration tests in `crates/wafer-run/tests/validation_test.rs` (new file)

- `start_fails_on_missing_required_config` — registers a block with a required key, omits value, asserts `start()` returns `Err` whose message names the block and key.
- `start_succeeds_when_all_required_present` — happy path with all required values provided.
- `call_block_rejects_wrong_action_for_interface` — a block advertising `database@v1` receives a message with action outside the database action set; asserts `InvalidArgument` error event.
- `call_block_allows_custom_interface` — block declares a custom interface; call succeeds; a single warning log is emitted.
- `existing_examples_still_pass` — smoke test constructing the setups from `examples/hello-world` and `examples/api-server`; asserts `start()` returns `Ok` and a representative `call_block` succeeds.

### Regression gate

`cargo test --workspace` plus the streaming e2e suite and the 15 WRAP tests in `wafer-block/src/wrap.rs` must all pass unchanged.

### Test utility

Log capture for the warn-once test uses the `tracing-test` crate as a dev-dependency. This avoids a handwritten log-capture helper and integrates cleanly with the existing `tracing` setup.

## Risks

1. **Audit misses a block.** If an existing block depends on empty-default-means-optional and the audit overlooks it, `start()` breaks for a downstream consumer (most likely solobase, the primary in-tree consumer). Mitigation: solobase's full startup (via its normal startup path, not just a unit test) is a mandatory acceptance check before the PR merges.
2. **Custom interface strings in the wild.** Any downstream that declares a block with a non-standard interface gets warnings on every startup. Mitigation: the warn-once mechanism limits the spam to one line per block per process.
3. **Interface spec completeness.** If an existing interface spec in `wafer_block::interfaces::all()` omits an action that a block is legitimately using, the validator rejects a valid dispatch. Mitigation: the audit pass also surveys every `call_block` in the workspace and cross-references against the declared interface spec before enabling the check.

## Rollout

Single branch, staged commits:

1. Land `runtime/validation.rs` with both functions + unit tests, not yet wired in.
2. Audit pass across all built-in blocks; commit any `ConfigVar` fixes discovered.
3. Wire `validate_config_presence` into `Wafer::start()`; run workspace tests + solobase startup.
4. Wire `validate_action_interface` into `RuntimeContext::call_block`; run workspace tests + solobase exercise.
5. Add `tests/validation_test.rs` integration suite.
6. Update relevant rustdoc on `Wafer::start()` and `call_block` to document the new validations.

No feature flag. The audit (step 2) is the safety gate.

## Open questions

None at spec time.
