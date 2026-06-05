# wafer-common

Source of the **TypeScript** constants used by the `wafer-client-js` SDK.

> **Rust and Go constants are hand-maintained, not generated.** The authoritative
> Rust definitions live in the `wafer-block` crate (`common/service_names.rs`,
> `meta.rs`, and the `ErrorCode` enum in `core_types.rs`) and are deliberately
> ahead of these TOMLs (they carry the vector/embedding/llm/image/auth services
> the TOMLs don't). Do **not** treat the TOMLs as the Rust/Go source of truth.
> This generator drives the TypeScript SDK only.

## Structure

- `definitions/` — TOML files defining the shared constants (consumed by the TS generator)
- `codegen/` — Python script + TypeScript template
- `generated/typescript/` — Auto-generated TypeScript constants (mirrored into
  `packages/wafer-client-js/src/generated/`)

## Regenerating

```bash
python3 wafer-common/codegen/generate.py
```

Requires Python 3.11+ (for `tomllib`), or install `tomli` for older versions.

## Definitions

| File | Contents |
|------|----------|
| `error_codes.toml` | gRPC-style canonical error codes (17 codes) |
| `meta_keys.toml` | Metadata key constants (request, response, auth, etc.) |
| `service_names.toml` | Service names and operation constants |
| `actions.toml` | Block result action values |
| `instance_modes.toml` | Block instance mode values |
| `lifecycle_types.toml` | Lifecycle event types |
