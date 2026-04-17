# Runtime correctness audit — ConfigVar empty defaults

**Date:** 2026-04-17
**Related plan:** docs/plans/2026-04-17-runtime-correctness.md (Tasks 5, 6)
**Related spec:** docs/specs/2026-04-17-runtime-correctness-design.md

## Summary

| Count | Category |
|---|---|
| 0 | Total empty-default declarations surveyed |
| 0 | Genuinely required (keep as-is) |
| 0 | Silently optional (fix before validator lands) |
| 0 | `auto_generate` already set (no-op for validator) |

## Search methodology

Three pattern searches were run across `crates/`, `sdks/`, and `examples/`:

1. `ConfigVar::new\("[A-Z_]+",\s*"[^"]*",\s*""` — literal empty third argument to `ConfigVar::new`.
2. `default_value\(""\)` — builder-method call setting empty default.
3. `default:\s*""` — struct-literal form with empty default.

All three searches returned zero hits in production block code. The only `ConfigVar::new(..., "")` calls found are in unit-test fixtures inside `crates/wafer-run/src/runtime/validation.rs` (lines 216, 236, 245, 255, 256, 259). Those are intentional test inputs for the `collect_missing_config` function itself and are not block declarations subject to the validator.

## Block-by-block survey

Every block in the workspace was checked for whether its `info()` method calls `.config_keys(vec![...])`. The full list of blocks surveyed:

| Block name | File | Uses `.config_keys()`? |
|---|---|---|
| `wafer-run/s3` | `crates/wafer-block-s3/src/lib.rs` | No |
| `wafer-run/postgres` | `crates/wafer-block-postgres/src/lib.rs` | No |
| `wafer-run/database` | `crates/wafer-core/src/service_blocks/database.rs` | No |
| `wafer-run/storage` | `crates/wafer-core/src/service_blocks/storage.rs` | No |
| `wafer-run/crypto` | `crates/wafer-core/src/service_blocks/crypto.rs` | No |
| `wafer-run/network` | `crates/wafer-core/src/service_blocks/network.rs` | No |
| `wafer-run/logger` | `crates/wafer-core/src/service_blocks/logger.rs` | No |
| `wafer-run/config` | `crates/wafer-core/src/service_blocks/config.rs` | No |
| `wafer-run/http-listener` | `crates/wafer-block-http-listener/src/lib.rs` | No |
| `wafer-run/web` | `crates/wafer-block-web/src/lib.rs` | No |
| `wafer-run/router` | `crates/wafer-block-router/src/lib.rs` | No |
| `wafer-run/cors` | `crates/wafer-block-cors/src/lib.rs` | No |
| `wafer-run/security-headers` | `crates/wafer-block-security-headers/src/lib.rs` | No |
| `wafer-run/ip-rate-limit` | `crates/wafer-block-ip-rate-limit/src/lib.rs` | No |
| `wafer-run/monitoring` | `crates/wafer-block-monitoring/src/lib.rs` | No |
| `wafer-run/auth-validator` | `crates/wafer-block-auth-validator/src/lib.rs` | No |
| `wafer-run/iam-guard` | `crates/wafer-block-iam-guard/src/lib.rs` | No |
| `wafer-run/readonly-guard` | `crates/wafer-block-readonly-guard/src/lib.rs` | No |
| `wafer-run/inspector` | `crates/wafer-block-inspector/src/lib.rs` | No |

No entries in `sdks/` or `examples/` use `ConfigVar` at all.

## Entries

None. No block in the workspace declares a `ConfigVar` with an empty default. The `ConfigVar` / `BlockConfigKey` type and its `.config_keys()` builder method on `BlockInfo` exist in the type system but are not yet used by any production block.

## Observation: blocks read config without declaring ConfigVar

Several blocks read configuration at runtime (via `BlockConfig::from_event`, `config_get`, or `env_or`) without declaring those keys in `BlockInfo::config_keys`. This means the forthcoming Task 9 validator will have nothing to check for these blocks today — but it also means the admin UI and static analysis have no visibility into what config those blocks expect.

Specific examples:

- **`wafer-run/postgres`** (`crates/wafer-block-postgres/src/lib.rs:100`) reads `DATABASE_URL` via `config.env_or("DATABASE_URL", "url")` and returns a hard error if absent. This is a genuinely required key with no declaration.
- **`wafer-run/s3`** (`crates/wafer-block-s3/src/lib.rs:67–78`) reads `STORAGE_BUCKET`, `STORAGE_PREFIX`, `STORAGE_ENDPOINT`, `STORAGE_REGION` with `unwrap_or_else` fallbacks. These are silently optional by code behavior.
- **`wafer-run/http-listener`** (`crates/wafer-block-http-listener/src/lib.rs:340`) reads `listen` via `config.str("listen")` and silently does nothing if it is empty (the `bind()` method returns early when `listen.is_empty()`).

These are not in scope for Task 6 (which fixes empty-default `ConfigVar` declarations), but they represent the *next* gap: blocks with undeclared required or optional config keys. Fixing them would mean adding `.config_keys(vec![...])` calls with the appropriate defaults in each block's `info()` — a separate follow-up task.

## Combined fix list (for Task 6)

No fixes required. Zero empty-default `ConfigVar` declarations were found.

## Note for Task 9 (config validator)

When the `Wafer::start()` validator lands it will have no blocks to reject, because no block currently populates `BlockInfo::config_keys`. The validator is still correct and future-safe: any new block that adds a `ConfigVar` with `default: ""` and `auto_generate: false` will be caught immediately.

The observation above (undeclared required/optional config) is a higher-priority correctness gap than the validator itself. Consider a follow-up task to add `BlockConfigKey` declarations to at minimum `wafer-run/postgres` (genuinely required `DATABASE_URL`) and `wafer-run/s3` (four optional keys with sensible defaults).

---

# Call-site / interface audit (Task 7)

**Date:** 2026-04-17
**Related plan tasks:** 7 (this audit), 8 (apply fixes), 11 (wire validator)

## Valid-action reference

Extracted from `crates/wafer-block/src/interfaces.rs` at commit 26516df:

| Interface | Actions |
|---|---|
| `middleware@v1` | (empty — action-agnostic) |
| `http-handler@v1` | `retrieve`, `create`, `update`, `delete` |
| `router@v1` | (empty — action-agnostic; delegates to handlers) |
| `http-listener@v1` | (empty — action-agnostic) |
| `database@v1` | `database.get`, `database.list`, `database.create`, `database.update`, `database.delete`, `database.count`, `database.query_raw`, `database.exec_raw`, `database.sum` |
| `storage@v1` | `storage.put`, `storage.get`, `storage.delete`, `storage.list`, `storage.create_folder`, `storage.delete_folder`, `storage.list_folders` |
| `crypto@v1` | `crypto.hash`, `crypto.compare_hash`, `crypto.sign`, `crypto.verify`, `crypto.random_bytes` |
| `http-client@v1` | `network.do` |
| `logger@v1` | `logger.debug`, `logger.info`, `logger.warn`, `logger.error` |
| `config@v1` | (empty — action-agnostic) |
| `service@v1` | (empty — action-agnostic) |

Note: `vector@v1` and `embedding@v1` are referenced by `wafer-run/vector` but do not appear in `interfaces::all()`.

## Survey

Full list of `call_block` sites audited, with classification:

| # | Call site (file:line) | Target block | Target interface | Action used | Classification | Resolution |
|---|---|---|---|---|---|---|
| 1 | `crates/wafer-block-router/src/lib.rs:133` | `<dynamic>` (from route config `route.block`) | varies (handler blocks, typically `http-handler@v1`) | `<dynamic>` (caller's `msg.action()` passed through as-is) | dynamic | review (dynamic) — see §Dynamic below |
| 2 | `crates/wafer-core/src/clients/mod.rs:108` | `<dynamic>` (parameter `block`) — concrete values: `"wafer-run/database"`, `"wafer-run/storage"`, `"wafer-run/crypto"`, `"wafer-run/network"`, `"wafer-run/logger"`, `"wafer-run/config"`, `"wafer-run/vector"`, `<embedding block>` | `database@v1` / `storage@v1` / `crypto@v1` / `http-client@v1` / `logger@v1` / `config@v1` / `vector@v1` / `<unknown>` | `<dynamic>` (`kind` param) — always set to a `ServiceOp::*` constant before call | dynamic | review (dynamic) — see §Dynamic below |
| 3 | `crates/wafer-run/src/wasm/wasmi_loader.rs:620` | `<dynamic>` (`pending.block_name` from WASM guest) | varies | `<dynamic>` (deserialized from WASM guest's serialized `Message`) | dynamic | review (dynamic) — see §Dynamic below |
| 4 | `crates/wafer-run/tests/streaming_e2e.rs:61` | `"test/upper"` | `http-handler@v1` | `"fwd"` (literal, `Message::new("fwd")`) | action-NOT-in-map | fix call: action `"fwd"` is not in `http-handler@v1`; replace with `"retrieve"` or appropriate handler action — or declare `test/upper` with `handler@v1` (action-agnostic). See §Mismatches |

Columns:
- **Call site**: file:line of the `ctx.call_block(...)` call.
- **Target block**: resolved block name string.
- **Target interface**: the interface string declared in the target block's `BlockInfo::new(..., interface, ...)`.
- **Action used**: literal action string if known, or `<dynamic>` if computed, with a note on origin.
- **Classification**: one of {action-agnostic, action-in-map, action-NOT-in-map, interface-unknown, dynamic}.
- **Resolution**: `N/A` | `fix call to use "<correct>"` | `add "<action>" to <interface> spec` | `review (dynamic)` | `review (unknown interface)`.

### Notes on row 2 decomposition

Row 2 covers the single `call_service` gateway function at `crates/wafer-core/src/clients/mod.rs:108`. All typed client modules funnel through it. Expanding by concrete `(block, kind)` pair:

| Sub-call | Target block | Target interface | Action | Classification |
|---|---|---|---|---|
| `database::get` | `wafer-run/database` | `database@v1` | `"database.get"` | action-in-map |
| `database::list` | `wafer-run/database` | `database@v1` | `"database.list"` | action-in-map |
| `database::create` | `wafer-run/database` | `database@v1` | `"database.create"` | action-in-map |
| `database::update` | `wafer-run/database` | `database@v1` | `"database.update"` | action-in-map |
| `database::delete` | `wafer-run/database` | `database@v1` | `"database.delete"` | action-in-map |
| `database::count` | `wafer-run/database` | `database@v1` | `"database.count"` | action-in-map |
| `database::query_raw` | `wafer-run/database` | `database@v1` | `"database.query_raw"` | action-in-map |
| `database::exec_raw` | `wafer-run/database` | `database@v1` | `"database.exec_raw"` | action-in-map |
| `database::sum` | `wafer-run/database` | `database@v1` | `"database.sum"` | action-in-map |
| `database::delete_by_filters` | `wafer-run/database` | `database@v1` | `"database.delete_where"` | action-NOT-in-map |
| `database::update_by_filters` | `wafer-run/database` | `database@v1` | `"database.update_where"` | action-NOT-in-map |
| `storage::put` | `wafer-run/storage` | `storage@v1` | `"storage.put"` | action-in-map |
| `storage::get` | `wafer-run/storage` | `storage@v1` | `"storage.get"` | action-in-map |
| `storage::delete` | `wafer-run/storage` | `storage@v1` | `"storage.delete"` | action-in-map |
| `storage::list` | `wafer-run/storage` | `storage@v1` | `"storage.list"` | action-in-map |
| `storage::create_folder` | `wafer-run/storage` | `storage@v1` | `"storage.create_folder"` | action-in-map |
| `storage::delete_folder` | `wafer-run/storage` | `storage@v1` | `"storage.delete_folder"` | action-in-map |
| `storage::list_folders` | `wafer-run/storage` | `storage@v1` | `"storage.list_folders"` | action-in-map |
| `crypto::hash` | `wafer-run/crypto` | `crypto@v1` | `"crypto.hash"` | action-in-map |
| `crypto::compare_hash` | `wafer-run/crypto` | `crypto@v1` | `"crypto.compare_hash"` | action-in-map |
| `crypto::sign` | `wafer-run/crypto` | `crypto@v1` | `"crypto.sign"` | action-in-map |
| `crypto::verify` | `wafer-run/crypto` | `crypto@v1` | `"crypto.verify"` | action-in-map |
| `crypto::random_bytes` | `wafer-run/crypto` | `crypto@v1` | `"crypto.random_bytes"` | action-in-map |
| `network::do_request` | `wafer-run/network` | `http-client@v1` | `"network.do"` | action-in-map |
| `network::do_request_via` | `<dynamic>` (caller-provided `block`) | unknown | `"network.do"` | dynamic / interface-unknown |
| `logger::debug/info/warn/error` | `wafer-run/logger` | `logger@v1` | `"logger.debug"` / `"logger.info"` / `"logger.warn"` / `"logger.error"` | action-in-map |
| `config::get` | `wafer-run/config` | `config@v1` | `"config.get"` | action-agnostic interface |
| `config::set` | `wafer-run/config` | `config@v1` | `"config.set"` | action-agnostic interface |
| `vector::create_index` | `wafer-run/vector` | `vector@v1` | `"vector.create_index"` | interface-unknown |
| `vector::delete_index` | `wafer-run/vector` | `vector@v1` | `"vector.delete_index"` | interface-unknown |
| `vector::upsert` | `wafer-run/vector` | `vector@v1` | `"vector.upsert"` | interface-unknown |
| `vector::query` | `wafer-run/vector` | `vector@v1` | `"vector.query"` | interface-unknown |
| `vector::delete` | `wafer-run/vector` | `vector@v1` | `"vector.delete"` | interface-unknown |
| `vector::count` | `wafer-run/vector` | `vector@v1` | `"vector.count"` | interface-unknown |
| `vector::embed` | `<dynamic>` (caller-provided `embedding_block`) | `embedding@v1` (declared by that block) | `"embedding.embed"` | interface-unknown |

## Mismatches requiring action before Task 11 lands

### Mismatch 1 — `database.delete_where` missing from `database@v1`

`crates/wafer-core/src/clients/database.rs` (via `call_service` at `crates/wafer-core/src/clients/mod.rs:108`) — `database::delete_by_filters()` sends action `"database.delete_where"` (`ServiceOp::DATABASE_DELETE_WHERE`) to `wafer-run/database` which declares `database@v1`. The action `"database.delete_where"` is not in the `database@v1` action map.

Fix: add `"database.delete_where"` to `database_v1()` in `crates/wafer-block/src/interfaces.rs` — the operation is legitimate (bulk-delete by filter), it was just never added to the spec when `delete_by_filters` was implemented.

### Mismatch 2 — `database.update_where` missing from `database@v1`

Same file / path. `database::update_by_filters()` sends action `"database.update_where"` (`ServiceOp::DATABASE_UPDATE_WHERE`) to `wafer-run/database`. The action `"database.update_where"` is not in the `database@v1` action map.

Fix: add `"database.update_where"` to `database_v1()` in `crates/wafer-block/src/interfaces.rs` — same reason as above; the operation is legitimate.

### Mismatch 3 — `"fwd"` is not an `http-handler@v1` action (test block)

`crates/wafer-run/tests/streaming_e2e.rs:61` — `PipeBlock::handle` calls `ctx.call_block("test/upper", Message::new("fwd"), ...)`. `test/upper` declares interface `"handler@v1"` (note: the string literal in `BlockInfo::new` is `"handler@v1"`, not `"http-handler@v1"`). Neither `"handler@v1"` nor `"http-handler@v1"` contains `"fwd"`.

`"handler@v1"` is not in `interfaces::all()` so the validator will hit the warn-once / allow path for that interface. However, the intent is a test-internal pass-through action; if the interface were recognised the action would fail. Strictly this is:

- If `"handler@v1"` stays unknown: validator allows it (warn-once). No runtime failure. Still flagged for hygiene.
- If `"handler@v1"` is later registered as an alias for `"http-handler@v1"`: it would be a hard mismatch.

Resolution options (either acceptable):
1. Change the test block to declare `"http-handler@v1"` and use `Message::new("retrieve")` (most consistent).
2. Change the test block to declare a service-type interface (`"service@v1"`) which is action-agnostic. The block is not actually an HTTP handler; it just transforms data. This is more accurate.

## Dynamic action sources — spot-check

### Row 1 — `wafer-block-router/src/lib.rs:133`

The router passes the incoming message's original action (e.g. `"retrieve"`, `"create"`, `"update"`, `"delete"`) through unchanged to the handler block. Route config declares which actions (`route.actions`) are accepted and which handler block to dispatch to. By design, only messages whose action is in the route's allowed-actions list reach a given handler. Handler blocks typically declare `http-handler@v1`, whose action map is exactly `{retrieve, create, update, delete}`.

The HTTP listener (`wafer-block-http-listener`) normalises HTTP methods to those four action strings before dispatching, so in practice the realistic value set at the `call_block` boundary is `{retrieve, create, update, delete}` — all four are in the `http-handler@v1` map. Additionally, the router's own `normalize_action` function maps `OPTIONS` to `"execute"`, which is **not** in `http-handler@v1`. However, `"execute"` would only reach the handler if a route explicitly lists `actions: ["execute"]`; the router does not reject it, so it is a latent mismatch depending on route config. This is a configuration-time concern, not a code defect.

Conclusion: statically safe for the common case (`retrieve/create/update/delete`). The `"execute"` / OPTIONS edge case is a risk in user-written flow configs, not in the codebase. No code change required.

### Row 2 — `crates/wafer-core/src/clients/mod.rs:108`

Every call to `call_service` supplies the `kind` argument from a `ServiceOp::*` constant. There are no computed or user-supplied action strings at these call sites. The full set of possible values is the closed set of `ServiceOp` constants. As shown in the sub-call table above, all are either action-in-map (for the interfaces that have action maps) or covered by action-agnostic interfaces (`config@v1`), except for `database.delete_where`, `database.update_where` (mismatches logged above), and the vector/embedding calls (unknown-interface, logged below).

The one exception is `network::do_request_via`, which accepts a caller-provided `block` name. The action is always `ServiceOp::NETWORK_DO_REQUEST` = `"network.do"`. If the caller-provided block declares `http-client@v1`, that action is valid. If it declares something else, the validator would check against whatever interface that block has. This is a valid extensibility pattern, not a mismatch.

### Row 3 — `crates/wafer-run/src/wasm/wasmi_loader.rs:620`

The WASM guest serialises a `Message` struct (including its `kind` field) and passes it to the host's `__wafer_host_call_block` import. The host deserialises the message, resolves the target block, and calls `ctx.call_block`. Both the target block name and the action are fully controlled by the WASM guest's source code. This is the intended extensibility mechanism for WASM blocks.

From a static-analysis standpoint this is fully dynamic: any block written to target `database@v1` and sending `"database.get"` is fine; a buggy WASM block sending `"database.frobnicate"` would be caught by the Task 11 validator at runtime. No action is needed in the source code here — the validator is the correct enforcement point.

## Unknown interfaces

The following blocks declare an interface string not present in `interfaces::all()`:

| Block | Declared interface | Notes |
|---|---|---|
| `wafer-run/vector` (`crates/wafer-core/src/service_blocks/vector.rs`) | `"vector@v1"` | No `vector_v1()` function exists in `interfaces.rs` and it is not included in `all()`. The validator will warn-once and allow. Six action constants exist: `vector.create_index`, `vector.delete_index`, `vector.upsert`, `vector.query`, `vector.delete`, `vector.count`. |
| `<embedding blocks>` (e.g. `wafer-block-fastembed`) | `"embedding@v1"` | No `embedding_v1()` in `interfaces.rs`. One action constant: `embedding.embed`. |
| `test/echo`, `test/upper`, `test/pipe`, `test/error`, `test/drop`, `test/multi` (`crates/wafer-run/tests/streaming_e2e.rs`) | `"handler@v1"` | Not in `interfaces::all()`. These are test-only blocks. The validator would warn-once and allow. The correct interface string for HTTP handler blocks is `"http-handler@v1"`. |

Resolution for `vector@v1` and `embedding@v1`: add `vector_v1()` and `embedding_v1()` functions to `crates/wafer-block/src/interfaces.rs` and include them in `all()`. This is not required before Task 11 lands (the validator will warn-once and allow), but it is the right long-term fix and would eliminate the unknown-interface warnings.

Resolution for `"handler@v1"` in test blocks: cosmetic — change to `"http-handler@v1"` or `"service@v1"` for correctness. No runtime consequence since the validator warns and allows.
