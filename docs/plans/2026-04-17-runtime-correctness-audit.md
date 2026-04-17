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
