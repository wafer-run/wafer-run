# Block capabilities: guest-declared + header policy

**Date:** 2026-04-18
**Status:** Proposed
**Scope:** Spec 2B of 3 in the wafer-run hardening initiative. Parent: security hardening (Spec 2). Builds on Spec 2A (security-block tests).

## Context

Today, `BlockCapabilities` exists in `crates/wafer-block/src/capabilities.rs` with nine fields (collections, raw_sql, storage_folders, crypto, network, network_allow, config, config_keys, callable_blocks) but is only ever imposed externally — typically by passing `BlockCapabilities::none()` via `load_with_capabilities` for remote WASM loads. WASM blocks themselves cannot declare what they need; operators cannot narrow by config; and `sanitize_guest_meta` strips security headers unconditionally from every WASM response regardless of the block's intent.

Consequences:
- A WASM-based auth block cannot set `Set-Cookie`, even if the operator wants it to.
- A block's capability requirements are implicit — discoverable only by reading source code.
- Least-privilege-by-code is inverted: the runtime decides what the block can do, not the block asking for what it needs.

Spec 2B closes this with three tightly-coupled changes: guests declare required capabilities, operators can narrow (never widen) via config, and the header-policy boolean asymmetry in `sanitize_guest_meta` is replaced with explicit per-header allow/deny arrays.

## Goals

- WASM blocks declare required capabilities in `BlockInfo`, propagated through the existing `__wafer_info` export and surfaceable to the inspector.
- The `#[wafer_block]` macro accepts a nested `capabilities(...)` argument so blocks opt in ergonomically.
- Operators narrow declared capabilities via a reserved `capabilities` subkey in block config. Intersection rules make narrowing the only direction.
- The current single-boolean `sanitize_guest_meta` gate is replaced with a `HeaderPolicy` sub-struct: two allowlists (readable/writable) for sensitive headers and one denylist extension (masked).
- Native Rust blocks can also declare `capabilities` as documentation / inspector metadata — the runtime does not enforce them on native blocks (preserves the existing trust model).
- Documentation in both `wafer-site` and `solobase` explicitly surfaces the native-vs-WASM enforcement asymmetry.

## Non-goals

- Capability enforcement on native blocks. They remain trusted in-process; declarations are documentation-only for native. A future spec could lift this if desired.
- Per-message dynamic capability grants (e.g., "this specific request is trusted"). This spec is static per-block.
- Signed capability manifests. Overkill for current needs.
- A richer inspector UI dedicated to capabilities. `BlockInfo.capabilities` is surfaced via the existing inspector `/blocks` endpoint; a richer view is a separate effort.
- Backward compatibility with pre-Spec-2B WASM blocks. We are in active development; any block rebuilt against the new SDK adopts the new schema.

## Design

### 1. `BlockCapabilities` + `HeaderPolicy`

In `crates/wafer-block/src/capabilities.rs`:

```rust
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BlockCapabilities {
    pub collections: HashSet<String>,
    pub raw_sql: bool,
    pub storage_folders: HashSet<String>,
    pub crypto: bool,
    pub network: bool,
    pub network_allow: Vec<String>,
    pub config: bool,
    pub config_keys: HashSet<String>,
    pub callable_blocks: HashSet<String>,
    /// NEW: per-header read/write/mask policy.
    #[serde(default)]
    pub headers: HeaderPolicy,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HeaderPolicy {
    /// Sensitive inbound headers the block may READ.
    /// Default-denied set (see `default_sensitive_headers()`) is masked
    /// unless explicitly listed here. Example: `["authorization"]`.
    #[serde(default)]
    pub readable: Vec<String>,

    /// Sensitive outbound headers the block may WRITE.
    /// Same default-deny; `writable` overrides per-header.
    /// Example: `["set-cookie"]`.
    #[serde(default)]
    pub writable: Vec<String>,

    /// Additional headers to mask beyond the default set. Applies to both
    /// directions. Operator-facing extension for app-specific sensitive
    /// headers (e.g., `["x-internal-token"]`).
    #[serde(default)]
    pub masked: Vec<String>,
}
```

Plus a new method on `BlockCapabilities`:

```rust
impl BlockCapabilities {
    /// Intersect: narrower wins for allowlists; denylists union (stricter wins).
    /// Used when combining declared caps with operator config and any host-
    /// supplied caps.
    pub fn intersect(&self, other: &Self) -> Self { /* ... */ }
}
```

Semantics:
- Booleans (`raw_sql`, `crypto`, `network`, `config`): logical AND.
- HashSet allowlists (`collections`, `storage_folders`, `config_keys`, `callable_blocks`): set intersection. `{"*"}` is a wildcard sentinel that intersects to the other side.
- Vec allowlist (`network_allow`): intersection (narrower wins; cannot extend).
- `HeaderPolicy::readable`, `HeaderPolicy::writable`: intersection.
- `HeaderPolicy::masked`: **union** (stricter wins; any contributor can add masking).

### 2. `BlockInfo::capabilities`

In `crates/wafer-block/src/types.rs`:

```rust
pub struct BlockInfo {
    // ... existing fields ...

    /// Capability declaration.
    ///
    /// For WASM blocks: carried in the JSON returned by `__wafer_info` and
    /// intersected with operator config at load time. Enforced at dispatch.
    ///
    /// For native blocks: documentation and inspector metadata only. Not
    /// enforced by the runtime. Native blocks continue to operate under
    /// the existing trust model.
    ///
    /// `None` means the block did not declare capabilities — the runtime
    /// applies the existing default (fully restricted for remote WASM loads
    /// via `BlockCapabilities::none()`; unrestricted for native).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<BlockCapabilities>,
}
```

A builder method `.capabilities(caps)` is added consistent with the existing `BlockInfo` builder pattern.

### 3. `#[wafer_block]` macro

Extend the attribute parser in `crates/wafer-block-macro/src/lib.rs` to accept a nested `capabilities(...)` argument:

```rust
#[wafer_block(
    name = "suppers-ai/auth",
    version = "0.1.0",
    interface = "middleware@v1",
    summary = "Authentication middleware",
    capabilities(
        crypto,
        config,
        collections = ["users", "sessions", "api_keys"],
        config_keys = ["SUPPERS_AI__AUTH__JWT_SECRET"],
        callable_blocks = ["wafer-run/crypto", "wafer-run/database"],
        headers(
            readable = ["authorization", "cookie"],
            writable = ["set-cookie"],
        ),
    )
)]
```

Grammar:
- Bare identifiers (e.g., `crypto`, `config`, `network`, `raw_sql`) → `field = true` for bool-typed fields.
- `field = [...]` populates HashSet or Vec fields from a string-literal array.
- Nested `headers(readable = [...], writable = [...], masked = [...])` maps to `HeaderPolicy`.
- Omitted fields default to `Default::default()` (false / empty).
- Omitting the whole `capabilities(...)` group yields `BlockInfo::capabilities = None` — the block declares nothing.

The macro generates `info()` to return a `BlockInfo` whose `.capabilities` is `Some(declared)` when the attribute was present, otherwise `None`.

### 4. Config schema + narrowing

Block config JSON gains a reserved `capabilities` subkey. Example:

```json
{
  "SUPPERS_AI__AUTH__JWT_SECRET": "hunter2",
  "capabilities": {
    "network": false,
    "network_allow": ["https://api.stripe.com/"],
    "collections": ["users"],
    "headers": {
      "writable": ["set-cookie"]
    }
  }
}
```

The runtime parses the reserved `capabilities` key out of the config (before the remainder flows to `ctx.config_get(...)`) and deserializes it as `BlockCapabilities` using the existing `#[derive(Deserialize)]`.

At block load / registration:

```
effective_caps = declared_caps.intersect(config_caps).intersect(host_caps)
```

Where:
- `declared_caps` = `BlockInfo::capabilities` from `__wafer_info`. Missing → `BlockCapabilities::unrestricted()` (intersection no-op).
- `config_caps` = parsed from the `capabilities` config subkey. Missing → `unrestricted()`.
- `host_caps` = optional caps passed via the existing `Wafer::load_with_capabilities(bytes, caps)` API. Missing → `unrestricted()`.

When an operator tries to widen beyond declared (e.g., declared `network: false`, config `network: true`), the intersection yields the narrower value (`false`) and the runtime emits a warn-once log:

```
WARN block=suppers-ai/auth: config widened field 'network' beyond declared — narrower declaration wins
```

This applies to every field where config widened.

### 5. Meta-key-to-header-name parsing

The existing `sanitize_guest_meta` uses substring matching (e.g., `k.contains("location")`). This is replaced with exact header-name matching against the header-name portion of the meta key.

Parser in a new helper function in `crates/wafer-run/src/wasm/wasmi_loader.rs`:

```rust
fn header_name_from_meta_key(key: &str) -> Option<String> {
    let lower = key.to_lowercase();
    if let Some(rest) = lower.strip_prefix("req.header.") {
        return Some(rest.to_string());
    }
    if let Some(rest) = lower.strip_prefix("resp.header.") {
        return Some(rest.to_string());
    }
    if lower == "resp.set_cookie" || lower.starts_with("resp.set_cookie.") {
        return Some("set-cookie".to_string());
    }
    None
}
```

Meta keys that don't match these prefixes are not considered headers and are exempt from the policy (internal meta like `auth.user_id`, `trace_id`, etc.).

### 6. `sanitize_guest_meta` replacement

In `crates/wafer-run/src/wasm/wasmi_loader.rs`, `sanitize_guest_meta` is rewritten to take the block's effective `BlockCapabilities` and apply the header policy:

```rust
pub(crate) fn default_sensitive_headers() -> &'static HashSet<&'static str> {
    // authorization, cookie, set-cookie, location,
    // access-control-allow-origin, access-control-allow-credentials,
    // access-control-allow-methods, access-control-allow-headers,
    // access-control-expose-headers, access-control-max-age,
    // strict-transport-security, x-frame-options,
    // content-security-policy, content-security-policy-report-only
    &SENSITIVE
}

fn sanitize_outbound_meta(
    meta: Vec<MetaEntry>,
    caps: &BlockCapabilities,
    stripped_names: &mut Vec<String>,
) -> Vec<MetaEntry> {
    let default = default_sensitive_headers();
    meta.into_iter()
        .filter(|e| {
            let Some(name) = header_name_from_meta_key(&e.key) else {
                return true; // Not a header — pass through.
            };
            let is_sensitive = default.contains(name.as_str())
                || caps.headers.masked.iter().any(|m| m.eq_ignore_ascii_case(&name));
            if !is_sensitive {
                return true;
            }
            let allowed = caps.headers.writable.iter().any(|w| w.eq_ignore_ascii_case(&name));
            if !allowed {
                stripped_names.push(name);
                return false;
            }
            true
        })
        .collect()
}
```

A symmetric `sanitize_inbound_meta` uses `caps.headers.readable` instead. It runs on the `Message` before it reaches the WASM guest's `handle()`.

Both sanitizers feed into the warn-once logging: one warning per `(block_name, direction)` per process, listing the stripped header names:

```
WARN block=my-org/foo direction=outbound stripped=[set-cookie, location]: headers outside writable allowlist
```

### 7. Native-block enforcement (not)

Native blocks continue to bypass capability enforcement entirely. `BlockInfo::capabilities` on a native block is stored for inspector display and documentation; it is not consulted at dispatch and does not affect `sanitize_*` calls (which are WASM-only code paths).

This is the D2 decision. The documentation (Section 8) surfaces this asymmetry explicitly.

### 8. Documentation requirements

Two documentation deliverables, both mandatory:

**`wafer-site`** (under `crates/wafer-site/`) — new page "Block capabilities":

- The capability model overview: declare, narrow, enforce.
- Side-by-side example: a WASM block declaring `capabilities(collections = ["users"], crypto)` vs a native block with the same declaration. Explicit callout: **the WASM declaration is enforced at dispatch; the native declaration is documentation-only** and shown in the inspector.
- `HeaderPolicy` section: the default-denied header list, the `readable`/`writable`/`masked` semantics, a worked example.
- Config narrowing: the `"capabilities": { ... }` subkey and the intersection rules. Explanation of why only narrowing is permitted.

**`solobase` docs** (in solobase's own `docs/` tree) — new section in the blocks reference:

- The four security-sensitive solobase blocks (auth, iam-guard, readonly-guard, ip-rate-limit) documented with their intended `HeaderPolicy` declarations once they adopt Spec 2B.
- Operator-facing walkthrough: read a block's declared capabilities via the inspector, decide which to narrow, write the config, observe effective caps.
- Explicit callout: **native blocks bypass enforcement**. Link to the wafer-site page for the full model.

Both sites get short rustdoc pointers from `BlockCapabilities` and `BlockInfo::capabilities` back to the relevant doc pages.

## Testing

### Unit tests in `crates/wafer-block/src/capabilities.rs`

- `intersect_booleans_and` — logical AND per boolean.
- `intersect_collections_set_intersection` — `{"a", "b"}` ∩ `{"b", "c"}` = `{"b"}`.
- `intersect_wildcard_sentinel` — `{"*"}` ∩ `{"b"}` = `{"b"}`; `{"*"}` ∩ `{"*"}` = `{"*"}`.
- `intersect_network_allow_vec_intersection` — prefix allowlists intersect.
- `intersect_header_policy_allowlists_intersect` — `readable`, `writable` intersect.
- `intersect_header_policy_masked_unions` — `masked` unions.

### Unit tests in `crates/wafer-block-macro/tests/`

- `macro_accepts_full_capabilities` — full declaration parses to `Some(BlockCapabilities { ... })` with correct fields.
- `macro_omits_capabilities_yields_none` — declaration without `capabilities(...)` yields `None`.
- `macro_bool_shorthand` — `capabilities(crypto, network)` expands to the expected booleans.
- `macro_nested_headers` — `headers(readable = [...], writable = [...])` populates `HeaderPolicy`.

### Unit tests in `crates/wafer-run/src/wasm/wasmi_loader.rs`

- `header_name_from_meta_key_req_header` — `"req.header.authorization"` → `Some("authorization")`.
- `header_name_from_meta_key_resp_header` — `"resp.header.x-custom"` → `Some("x-custom")`.
- `header_name_from_meta_key_legacy_set_cookie` — `"resp.set_cookie"` / `"resp.set_cookie.foo"` → `Some("set-cookie")`.
- `header_name_from_meta_key_internal` — `"auth.user_id"` → `None`.
- `sanitize_outbound_strips_default_sensitive` — empty policy strips the full default set.
- `sanitize_outbound_respects_writable_allowlist` — `writable = ["set-cookie"]` lets Set-Cookie pass.
- `sanitize_inbound_respects_readable_allowlist` — `readable = ["authorization"]` lets Authorization pass.
- `sanitize_masked_denies_both_directions` — `masked = ["x-internal"]` strips `req.header.x-internal` and `resp.header.x-internal`.
- `sanitize_warn_once_per_block_per_direction` — log capture confirms single warn per (block, direction).

### Integration tests in `crates/wafer-run/tests/capabilities_e2e.rs` (new file)

Uses `wafer-test-support::WaferBuilder` from Spec 2A for test setup.

- `declared_caps_applied_when_no_config` — register a WASM block declaring `collections = ["users"]`; verify DB access to `users` works and access to `other` is denied.
- `operator_config_narrows_declared` — block declares `network`; config narrows to `network_allow = ["https://api.example.com/"]`; effective caps reflect the allowlist.
- `operator_config_cannot_widen_declared` — block declares `network: false`; config specifies `network: true`; effective caps is `false` and a warn log is emitted.
- `native_block_declares_but_not_enforced` — native block declares `collections = ["users"]`; runtime stores the declaration (visible via block info); calls to un-declared collections succeed at dispatch. Test confirms both properties.
- `header_policy_auth_block_roundtrip` — mock WASM "auth" block declaring `headers(readable = ["authorization"], writable = ["set-cookie"])`. Verify: Authorization reaches the block, Set-Cookie flows out, arbitrary other sensitive headers (e.g., x-frame-options) are stripped.

### Regression gate

- `cargo test --workspace` — all pre-Spec-2B tests continue to pass.
- `cargo clippy --workspace --no-deps -- -D warnings` — no new warnings.
- Existing unrelated WASM tests (`echo_block.wasm` and similar) continue to work.

## Risks

1. **Header-name parsing completeness.** If a meta key format we haven't enumerated carries a sensitive header, it could slip through. Mitigation: parser is a single function with its own unit tests; adding new prefixes is a one-line change. The three prefixes listed (`req.header.`, `resp.header.`, `resp.set_cookie` legacy) cover all forms currently in use across the codebase.
2. **Default-denied list completeness.** If a new security-sensitive header lands in HTTP standards or we discover one, the list must be updated. Mitigation: the list is a single `const` in wafer-run; updates are one-file changes with unit-test coverage.
3. **Operator confusion around intersection.** The "widen is silently narrowed" behavior is non-obvious without logs. Mitigation: the warn-once log explicitly names the field and explains the narrowing-wins rule.
4. **Macro ergonomics.** A large `capabilities(...)` block in an attribute is noisy. Mitigation: the syntax matches the existing `requires = [...]` pattern; bare-identifier shorthand for bools keeps simple cases concise.
5. **Native-block drift.** Native declarations are not enforced, so a declaration can diverge from reality without breaking anything. Mitigation: inspector surfaces the declaration; divergence surfaces at audit time rather than silently. Documented as part of the asymmetry callout.
6. **Scope growth from the original B3 single-boolean.** The `HeaderPolicy` model is richer than a single `can_set_security_headers` bool. Acceptable tradeoff for the finer control surface requested.

## Rollout

Single branch `feat/capabilities`, one commit per step:

1. Add `HeaderPolicy` struct + `headers` field on `BlockCapabilities` + unit tests.
2. Add `BlockCapabilities::intersect` + unit tests covering boolean AND, set intersection, wildcard sentinel, Vec intersection, and `HeaderPolicy` semantics.
3. Add `BlockInfo::capabilities: Option<BlockCapabilities>` field + serialize/deserialize tests.
4. Add `header_name_from_meta_key` helper + unit tests.
5. Rewrite `sanitize_guest_meta` as `sanitize_outbound_meta(meta, caps, &mut stripped)`; add `sanitize_inbound_meta` for the read-side; add warn-once state threaded through the loader.
6. Parse the `capabilities` subkey from block config in `resolver.rs`; wire effective-caps computation at load/registration time.
7. Extend `#[wafer_block]` macro to accept `capabilities(...)` + macro expansion tests.
8. Integration tests in `crates/wafer-run/tests/capabilities_e2e.rs`.
9. Documentation: `wafer-site` page + solobase docs section + rustdoc cross-references.

Steps 1–5 land the capability data model and enforcement mechanics. Step 6 wires config narrowing. Step 7 gives authors ergonomic declaration. Steps 8–9 lock in the contract and surface it to readers.

## Success criteria

- `cargo test --workspace` — no regressions; 15–25 new tests across unit + integration.
- `cargo clippy --workspace --no-deps -- -D warnings` — clean.
- A new WASM block using `#[wafer_block(capabilities(...))]` has its declared caps read from `__wafer_info`, intersected with operator config, and enforced at dispatch.
- `sanitize_guest_meta`-equivalent logic (now split into inbound and outbound) honors the `HeaderPolicy` allowlists and masked-list extension.
- `BlockInfo.capabilities` on a native block is observable via the inspector endpoint but does not change dispatch behavior.
- Documentation in `wafer-site` and `solobase` surfaces the native-vs-WASM enforcement asymmetry explicitly.

## Open questions

None at spec time.
