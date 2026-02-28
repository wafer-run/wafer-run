# wafer-common

Single source of truth for shared WAFER constants, error codes, and enums.

## Structure

- `definitions/` — TOML files defining all shared constants
- `codegen/` — Python script + templates that generate language-specific files
- `generated/` — Auto-generated Rust, Go, and TypeScript constant files

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
