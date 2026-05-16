# Changelog

## Unreleased

### Breaking changes

- `Wafer::new` now takes `Arc<dyn ConfigSource>` instead of returning
  `Result<Self, RuntimeError>` with no config arg. Embedders must implement
  `ConfigSource` (or use `StaticConfigSource` for tests). The return type is
  still `Result<Self, RuntimeError>`.
- `Wafer::resolve()` is renamed `Wafer::seal()`. Same call-site semantics
  (call once after `register_block` / `add_block_config`, before first
  dispatch). Composite/uses expansion, capability resolution, and snapshot
  finalization still happen there.
- `Wafer::start_without_bind` is removed. Use `Wafer::seal()` instead, which
  no longer runs the eager `lifecycle(Init)` walk — per-block `Init` runs
  lazily on first dispatch per isolate.
- Boot-time required-config validation is removed. Missing required keys
  no longer prevent boot; they surface as a 5xx on first dispatch of the
  affected block. Use `Wafer::validate_all_block_configs()` for an explicit
  health check (intended for `/_health` routes).
- `add_block_config()` JSON no longer flows into a block's `lifecycle(Init)`
  payload. It still participates in composite/uses expansion and is
  surfaced via `RuntimeContext::block_configs()`. Block init config comes
  from the registered `ConfigSource`.
- `RuntimeContext::make_context` gains an `init_breadcrumbs: InitStack`
  parameter. Update non-test in-tree callers if any exist (all in-tree
  callers were updated in this PR).
- `RuntimeContext` gains `slots` and `config_source` fields used by
  `dispatch_call` to lazily init callees of `call_block`. External code
  that constructs `RuntimeContext` literals (none today; the fields are
  `pub(crate)`) would need updating.

### Added

- `ConfigSource` trait + `EnvBlockConfig` + `ConfigError` + `StaticConfigSource`
  (`wafer_run::runtime::config_source`).
- `BlockSlot`, `InitializedState`, `InitError` (`wafer_run::runtime::slot`).
- `InitStack` for cycle detection (`wafer_run::runtime::init_stack`).
- `Wafer::init_block` / `init_block_with_stack` for lazy per-block init.
- `Wafer::validate_all_block_configs` returning `ValidationReport`.
- `WaferBuilder::config_source` setter.

### Refactored

- WRAP grant collection moved from `resolve()`-time to `register_block()`-time
  (per-block validation against the admin block). Typed grants
  (Network/Storage/Crypto) declared by a block that is registered before
  `set_admin_block` is called are deferred (logged + dropped at
  registration), then re-collected when `set_admin_block` runs a rescan
  of every registered block. External grants added via
  `Wafer::add_wrap_grants` are tracked separately and preserved across
  rescans. This accommodates the linkme registration order used by
  `WaferBuilder::build()`, where blocks are auto-registered before the
  embedder gets a chance to call `set_admin_block`.

### Migration

See `docs/migrations/lazy-block-init.md`.
