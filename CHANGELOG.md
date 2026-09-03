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
- `WebMcpRefusal::RecursiveSchema` and `WebMcpRefusal::OutputSchemaRecursive`
  are removed. A recursive endpoint schema is no longer a refusal: its cyclic
  definitions are published under `$defs` and referenced by `$ref`, so tools
  that used to be dropped now appear in the manifest. Two variants replace
  them: `CollidingDefinitions { names }` (two sources of one flat
  `inputSchema` keep definitions of the same name with different bodies — the
  tool is refused rather than misdescribed) and `SelectionNotFound` (a
  `ToolSelection` naming an endpoint no block declares). Matches on
  `WebMcpRefusal` must be updated; it stays `#[non_exhaustive]`.
- `BlockCapabilities::storage_folders` `Only` entries are folder PREFIXES, not
  exact object paths. An entry now admits itself and everything beneath it as
  a `/`-separated path, so `"uploads"` grants every key in `uploads/` (it
  previously granted only the literal resource `"uploads"`, which made
  folder-shaped grants unusable). Two rules bound the match: a resource with
  any empty, `.` or `..` segment is refused outright — nothing normalizes the
  path, so `site/jhg/../other` would otherwise pass a `site/jhg` grant while
  naming a sibling folder — and an `Only` entry that is empty or ends in `/`
  matches nothing. The storage handler rejects the same shapes one layer
  earlier with `InvalidArgument`. Operators who enumerated individual object
  paths to work around the old exact match should collapse them to the folder;
  operators who declared a folder expecting exact matching now grant its
  contents. `storage_folders` also narrows through
  `Allowlist::intersect_path_prefix` rather than a set intersection, so an
  override nested under a declared entry (or vice versa) survives as the
  narrower of the two instead of collapsing to deny-all.

### Added

- Structured schema operations on the database interface —
  `database.ensure_table`, `database.add_column`, `database.drop_table` and
  `database.table_exists` — taking a wire `TableDef` / `ColumnDef` the host
  converts to `wafer_schema` types and builds the statement from. Wrapped by
  `wafer_core::clients::database` and the Rust SDK.
- `BlockCapabilities::schema` (`bool`, default `false`, `true` in
  `unrestricted()`): gates the three structured write ops above via the new
  `wafer_block::wrap::SCHEMA_RESOURCE` (`__schema__`) sentinel, which follows
  the same WRAP rule as `__ddl__` — any attributable caller. It does NOT grant
  raw `database.ddl`, and `ddl` does not grant it: a sandboxed block can hold
  `schema: true, ddl: false` and still create its own tables. Overridable from
  block config like every other capability.
- `__wafer_host_codec` — an optional guest export negotiating the host-call
  payload codec. Returning `1` (JSON) makes the host transcode every host-call
  request body and response frame between JSON and MessagePack, so a
  dependency-free, std-only guest can drive the database / storage / config
  services. Attachments (`__wafer_host_stream_attach`) and attachment lookup
  remain MessagePack-only, and a JSON guest has no raw request-body path (the
  streaming-upload direction) — both answer `InvalidArgument`.
- `frame.encoding = raw` stream meta marker
  (`wafer_block::stream::raw_frames_marker`): a handler emits it to declare
  that every frame after it is opaque application bytes rather than a wire
  DTO, so a consumer that re-encodes frames forwards them verbatim. Emitted by
  `storage.get` / `storage.get_streaming` between the `ObjectInfo` header and
  the object body — no sniffing.
- `wafer_core::discovery::generate_webmcp_selected` + `ToolSelection`: build a
  page-scoped WebMCP manifest from an explicit endpoint allowlist (block,
  method, path, published name and description) instead of from whatever each
  block's author opted into globally. Runs the same name, auth and schema
  rules as the global manifest.
- `/openapi.json` hoists every schema's `$defs` into `components.schemas` and
  rewrites `#/$defs/X` references to `#/components/schemas/X`. Two different
  definitions that share a name are disambiguated with a content-hash suffix;
  identical bodies merge. A document with no `$defs` anywhere is unchanged.
- `cache_mode` config key on `wafer-run/web`: `"normal"` (default) or
  `"no-cache"`, which forces `Cache-Control: no-cache` on every response for
  sites edited live.
- `frame_ancestors` config key on `wafer-run/security-headers`: `"none"`
  (default) or `"self"`, which relaxes both the CSP `frame-ancestors`
  directive and `X-Frame-Options` for same-origin framing.
- `cross_origin_isolation` config key on `wafer-run/security-headers`:
  `"none"` (default: no `Cross-Origin-Opener-Policy` /
  `Cross-Origin-Embedder-Policy` headers, a true no-op) or `"credentialless"`
  / `"require-corp"`, both of which set `Cross-Origin-Opener-Policy:
  same-origin` and make the document `crossOriginIsolated`.
  `credentialless` keeps cross-origin no-cors subresources loadable — fetched
  without credentials — without the third party opting in; `require-corp`
  requires every cross-origin subresource to opt in via CORP or CORS. Lets a
  deployment that needs isolation (e.g. a threaded in-browser compiler) opt
  in per-response from the block that already owns response security
  headers; per the HTML spec, a document sending either value can only embed
  nested documents that also carry a compatible COEP.

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
