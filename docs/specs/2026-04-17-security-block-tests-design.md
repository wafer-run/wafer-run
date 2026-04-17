# Security-block test coverage

**Date:** 2026-04-17
**Status:** Proposed
**Scope:** Spec 2A of the wafer-run hardening initiative. Parent: security hardening (Spec 2), split into Spec 2A (this) and Spec 2B (capability model reform, separate spec).

## Context

The recent architectural review identified that four middleware blocks with security-critical responsibilities have **zero unit tests**:

- `wafer-block-auth-validator` — authenticates JWTs + API keys, sets `auth.user_id`, `auth.user_email`, `auth.user_roles` meta. Calls `wafer-run/database` (API key lookup) and `wafer-run/crypto` (JWT verify).
- `wafer-block-iam-guard` — role-based access check. Calls `wafer-run/database` (role lookup) with a meta-based fallback if the DB is unavailable.
- `wafer-block-readonly-guard` — blocks write actions when a `readonly` flag is set. Stateless; no external deps.
- `wafer-block-ip-rate-limit` — per-IP rate limiting with in-memory state. No external deps.

For a runtime designed to host other people's code, this coverage gap is the highest-risk area surfaced by the review. The dual-path logic in `auth-validator` (DB-then-meta) and `iam-guard` (DB-then-meta) is especially worth explicit testing — a silent fail-open on DB failure would be a security incident.

## Goals

- Ship unit tests covering the documented behavior of all four security blocks, including their failure modes.
- Ship one integration test composing the full auth → authz → rate-limit → readonly pipeline.
- Provide a shared, reusable test-support foundation so these tests are cheap to write and future block tests don't reinvent the same fixtures.
- Do all of this without putting test-only code paths or shortcuts into production source files.

## Non-goals

- Changes to the behavior of any of the four blocks. Tests pin existing behavior.
- Test helpers beyond the minimum the four security blocks need (no `FakeStorage`, `FakeNetwork`, etc. — add when a test actually needs them).
- Load tests, fuzz tests, property-based tests. Correctness-focused here.
- Security-header correctness tests — those belong with `wafer-block-security-headers`, not in Spec 2A.
- Any change to the capability model or `sanitize_guest_meta` — that's Spec 2B.

## Architecture

### New crate: `wafer-test-support`

Path: `crates/wafer-test-support/`. Regular workspace crate (compiled with `cargo test --workspace`), dependencies listed below. Consumed as `[dev-dependencies]` only — **never a runtime dependency of any production crate**.

Dependencies (regular):

- `wafer-block` — `Block` trait, `Message`, `BlockInfo`, `Context`, interface specs.
- `wafer-run` — `Wafer` (for `WaferBuilder`), `RuntimeContext`.
- `async-trait`, `serde_json`, `parking_lot` — standard infrastructure.
- `hmac`, `sha2`, `base64ct` — real HMAC-SHA256 for `FakeCrypto`'s JWT math.

Modules:

- `wafer_test_support::fake_db` — `FakeDb` struct (implements `Block`), `FailureMode` enum.
- `wafer_test_support::fake_crypto` — `FakeCrypto` struct (implements `Block`), `FailureMode` enum.
- `wafer_test_support::builder` — `WaferBuilder`.

### `FakeDb`

```rust
pub struct FakeDb {
    state: Arc<Mutex<FakeDbState>>,
}

struct FakeDbState {
    collections: HashMap<String, Vec<serde_json::Value>>,
    failure: FailureMode,
}

pub enum FailureMode {
    None,
    Unavailable,           // every call returns Internal error
    FailNextCall(u32),     // next N calls fail, then reset
}
```

Implements `Block` with `BlockInfo::new("test/fake-db", "0.1.0", "database@v1", "…")`. `handle()` dispatches on `msg.action()` and supports the minimum subset the security blocks actually use:

- `database.list` — returns all rows in the collection, honors simple equality filters in `data.filters`.
- `database.get` — returns one row by id.
- `database.create` — appends row with generated `id` if not present.
- `database.update`, `database.delete` — stubs returning `Ok`.
- `database.count` — returns row count matching filters.

Any unsupported action returns `WaferError::invalid_argument("fake-db: action X not implemented")` so fixture gaps surface loudly. If `failure != None`, every call returns `WaferError::internal("fake-db unavailable")` before dispatch.

Test-only control API (methods on the struct, not on the `Block` trait): `FakeDb::new()`, `FakeDb::seed(collection, rows)`, `FakeDb::set_failure(mode)`, `FakeDb::clear()`. These configure internal state on a type that production code never sees — they are not dispatch shortcuts.

### `FakeCrypto`

```rust
pub struct FakeCrypto {
    state: Arc<Mutex<FakeCryptoState>>,
}

struct FakeCryptoState {
    jwt_secret: Vec<u8>,
    failure: FailureMode,
}
```

`BlockInfo::new("test/fake-crypto", "0.1.0", "crypto@v1", "…")`. Handles:

- `crypto.jwt_sign` — input `{claims}`, returns `{token}` using **real HMAC-SHA256** with `state.jwt_secret`.
- `crypto.jwt_verify` — input `{token}`, returns `{valid, claims}` or `WaferError::unauthenticated("invalid signature")`.
- `crypto.hash` — SHA-256 hex (for API key hash comparison if the real auth-validator uses it).

Real HMAC matters: mocking verification would make auth-validator tests meaningless. A test that signs with secret A and passes the token to FakeCrypto configured with secret B sees an actual signature failure, matching prod.

Test-only control API: `FakeCrypto::new()`, `FakeCrypto::with_secret(s)`, `FakeCrypto::set_failure(m)`. No `sign_test_token` shortcut; tests produce tokens by dispatching `crypto.jwt_sign` through the runtime like production does.

### `WaferBuilder`

```rust
pub struct WaferBuilder { wafer: Wafer }

impl WaferBuilder {
    pub fn new() -> Self { Self { wafer: Wafer::new() } }

    // Registers FakeDb at "test/fake-db" and aliases "wafer-run/database" → "test/fake-db".
    pub fn with_fake_db(mut self, db: Arc<FakeDb>) -> Self { ... }

    // Registers FakeCrypto at "test/fake-crypto" and aliases "wafer-run/crypto" → "test/fake-crypto".
    pub fn with_fake_crypto(mut self, crypto: Arc<FakeCrypto>) -> Self { ... }

    pub fn with_block(mut self, name: &str, block: Arc<dyn Block>) -> Self { ... }
    pub fn with_config(mut self, block: &str, config: serde_json::Value) -> Self { ... }

    pub async fn build(self) -> Result<Arc<Wafer>, RuntimeError> { self.wafer.start().await }
}
```

Alias use is deliberate: `with_fake_db` uses `Wafer::add_alias("wafer-run/database", "test/fake-db")`. That's an existing production API (used by solobase's registration); tests reuse the same mechanism.

What is deliberately **not** in `WaferBuilder`:

- No `.assert_ok()` / `.assert_err(..)` sugar on the returned runtime. Tests inspect `OutputStream::collect_buffered()` directly.
- No `.run_with_json(name, json)` shortcut. Tests build real `Message`s.

Either shortcut would introduce a parallel path tests use but production doesn't — the kind of drift we want to avoid.

### `wafer-block-ip-rate-limit` clock seam

One small production change: thread a `Clock` trait through the rate-limit block.

```rust
pub trait Clock: Send + Sync { fn now(&self) -> Instant; }

pub struct SystemClock;
impl Clock for SystemClock { fn now(&self) -> Instant { Instant::now() } }
```

Default stays `SystemClock` for production. Constructor accepts an optional `Arc<dyn Clock>`. This replaces all direct `Instant::now()` calls inside the block. No behavior change in production.

Reason: the `window_reset_restores_budget` test needs to advance time deterministically. The alternative is `std::thread::sleep(window)`, which is slow and flaky. The trait seam is small and justified by the test requirement.

## Test matrix

All unit tests live in `#[cfg(test)] mod tests` within each block crate's `src/lib.rs`. The module is gated on `#[cfg(test)]` so it only compiles during test builds and has no effect on production artifacts.

### `wafer-block-auth-validator` — 6 tests

1. `missing_token_returns_unauthenticated` — no Authorization header, no cookie.
2. `valid_jwt_sets_auth_meta_and_continues` — signed JWT with `{sub, email, roles}`; asserts output meta contains `auth.user_id`, `auth.user_email`, `auth.user_roles`.
3. `invalid_jwt_signature_returns_unauthenticated` — token signed with a different secret than FakeCrypto holds; real HMAC rejects it.
4. `valid_api_key_sets_auth_meta` — seed DB with `api_keys` row; request with `X-API-Key` header; assert meta.
5. `unknown_api_key_returns_unauthenticated` — API key not in DB.
6. `db_unavailable_on_api_key_returns_internal_not_bypass` — `db.set_failure(Unavailable)`; assert error, not silent pass-through.

### `wafer-block-iam-guard` — 6 tests

1. `user_with_required_role_from_db_continues` — seed `iam_user_roles`.
2. `user_without_required_role_from_db_denies` — different role in DB; expect `PermissionDenied`.
3. `db_unavailable_falls_back_to_meta_roles` — DB down, matching role in `auth.user_roles` meta; expect continue.
4. `db_unavailable_meta_roles_missing_denies` — DB down, wrong/no role in meta; expect `PermissionDenied`.
5. `no_auth_meta_denies_regardless_of_db` — no `auth.user_id`; expect denial.
6. `required_role_defaults_to_admin_when_unconfigured` — config unset; expect the documented `admin` default. This test pins current behavior.

### `wafer-block-readonly-guard` — 4 tests

1. `readonly_off_write_action_continues` — `readonly=false`, action `create`.
2. `readonly_on_write_action_denies` — `readonly=true`, parametrised across `create`, `update`, `delete`.
3. `readonly_on_read_action_continues` — `readonly=true`, action `retrieve`.
4. `readonly_default_off` — config unset.

### `wafer-block-ip-rate-limit` — 5 tests

1. `under_limit_continues_with_remaining_meta` — first request; asserts `X-RateLimit-Remaining` meta.
2. `over_limit_denies_with_retry_after` — fire N+1 requests in one window; last one errors with `Retry-After` set.
3. `window_reset_restores_budget` — uses the injected clock to advance past the window; counter resets.
4. `disable_via_env_skips_entirely` — `RATE_LIMIT_IP=0`; all requests pass.
5. `distinct_ips_have_separate_buckets` — two IPs each under their own limit; both succeed.

### Integration test — `crates/wafer-run/tests/security_pipeline_e2e.rs` — 4 flows

Each composes all four blocks in order: `auth-validator` → `iam-guard` → `ip-rate-limit` → `readonly-guard` → handler.

1. `happy_path_authed_read_succeeds` — valid JWT, user has role, rate OK, not readonly, action `retrieve`. End-to-end success.
2. `unauthenticated_request_stops_at_auth` — no token. Error originates at `auth-validator`; subsequent blocks never run.
3. `authed_wrong_role_stops_at_iam` — valid JWT, user lacks required role. auth continues; iam denies.
4. `readonly_mode_blocks_writes_through_pipeline` — valid auth, role OK, rate OK, `readonly=true`, action `create`. readonly-guard denies.

## Error handling in tests

Tests inspect `OutputStream::collect_buffered().await` and destructure the terminal variant. For error assertions, match on `Err(TerminalNotResponse::Error(WaferError { code, message }))` and assert both:

- `code == ErrorCode::X` — exact.
- `message.contains("distinguishing substring")` — substring only, never exact match, so wording changes don't break tests.

## Rollout

Single branch `feat/security-tests`, one commit per step:

1. Scaffold `crates/wafer-test-support/` (Cargo.toml, empty module tree, workspace member).
2. `FakeDb` + its own self-tests (round-trip: seed, dispatch `database.retrieve`, verify).
3. `FakeCrypto` + self-tests (sign with secret A, verify with secret B fails).
4. `WaferBuilder` + self-test (build a runtime with just FakeDb, verify alias).
5. Add `Clock` trait to `wafer-block-ip-rate-limit`. Unit test of the seam. No behavior change.
6. Unit tests in `wafer-block-auth-validator` (6 tests).
7. Unit tests in `wafer-block-iam-guard` (6 tests).
8. Unit tests in `wafer-block-readonly-guard` (4 tests).
9. Unit tests in `wafer-block-ip-rate-limit` (5 tests).
10. Integration test `crates/wafer-run/tests/security_pipeline_e2e.rs` (4 flows).

Steps 1–4 land `wafer-test-support` as independently useful infrastructure; Spec 2B or future block tests can reuse it with no further work.

## Risks

1. **Fake drift.** If the real database service adds new action semantics (e.g., a new filter operator), `FakeDb`'s minimum-subset handling could misrepresent it, silently passing broken tests. Mitigation: Spec 1's action-interface validator already catches unknown actions at dispatch — renaming an action fails tests loudly. Semantic drift within a known action is accepted risk.
2. **HMAC algorithm divergence.** If the real crypto block defaults to a non-HMAC algorithm (e.g., RS256), `FakeCrypto`'s HMAC-only implementation won't match. Mitigation: during implementation, verify the real crypto block supports HS256; configure both sides explicitly in tests. Document the HS256 assumption in `FakeCrypto`'s rustdoc.
3. **Clock-seam scope creep.** Threading a `Clock` trait is a real production change, even if small. Mitigation: keep the trait one method (`fn now() -> Instant`), default impl unchanged, no externally visible behavior change. Add one unit test verifying the seam.

## Success criteria

- `cargo test --workspace` includes the 25 new tests (21 unit + 4 integration). All pass.
- `cargo clippy --workspace -- -D warnings` clean.
- No production-crate source file contains test-only code paths or conditional dispatch logic. `wafer-test-support` is not a runtime dependency of any production crate.
- Integration test exercises the four security blocks end-to-end through the real `Wafer` runtime and the real dispatch path (including the Spec 1 interface-action validator).

## Open questions

None at spec time.
