# Rust Best-Practices Review — wafer-run

Date: 2026-05-14
Reviewer: Claude (rust-best-practices skill, Apollo handbook)
Scope: `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
plus a chapter-by-chapter scan across all 30 crates.

This is a triage document. Findings are grouped by severity; each item names the
files/lines so it can be picked up individually. Strengths are summarised at the
end so we don't lose them on the next refactor.

---

## P0 — Clippy under default features + `--all-targets` (FIXED on `chore/clippy-clean`)

`cargo clippy --workspace --all-targets --locked -- -D warnings` originally
exited **101**. After the fixes on this branch it exits **0**.

The handbook recommends adding `--all-features` to that invocation. **That
doesn't apply to this workspace**: `wafer-core`'s `wasm-component` feature is
documented as mutually exclusive with the default async path
(`crates/wafer-core/Cargo.toml:8-13`). Enabling it via `--all-features` swaps
every `clients::*` function from `async fn(ctx: &dyn Context, …)` to a sync
`fn(…)`, but downstream callers in `wafer-block-web`, `wafer-block-network`
tests, and `wafer-core` tests still pass `ctx` and `.await` — those are
compile errors, not clippy warnings, and they're an architectural property of
the feature. CI today runs `cargo clippy --workspace -- -D warnings`
(`.github/workflows/ci-main.yml:45`), which is the correct invocation for this
repo. Adding `--all-targets` to CI is the only safe expansion.

### Fixes applied

- `crates/wafer-core/src/interfaces/image/service.rs:281` —
  `clippy::field_reassign_with_default`: rewrote `ModelCapabilities` test fixture
  using struct-update syntax.
- `crates/wafer-core/src/clients/image.rs`,
  `crates/wafer-core/src/clients/llm.rs`,
  `crates/wafer-core/src/clients/network.rs`,
  `crates/wafer-core/src/clients/mod.rs` — moved feature-only imports
  (`codec`, `ErrorCode`, `ResponseHeader`, `META_*`) under
  `#[cfg(not(feature = "wasm-component"))]` so the `wasm-component` build
  doesn't trip `unused_imports`.
- `crates/wafer-core/src/clients/mod.rs:254-271` — removed the dead
  `#[cfg(feature = "wasm-component")] call_service_streaming` stub. Every
  caller is gated to `not(wasm-component)`, so the stub had no path of
  reachability under any feature combination.
- `crates/wafer-run/tests/streaming_spike.rs:83` —
  `clippy::uninlined_format_args`: inlined the trailing `elapsed` argument.

**Recommendation:** add `--all-targets` to the CI clippy invocation so test
files (like `streaming_spike.rs`) are linted alongside library code. Leave
`--all-features` off — it's architecturally incompatible with the
`wasm-component` feature.

---

## P1 — Sync bridges in production (CLAUDE.md violation)

`wafer-run/CLAUDE.md` explicitly forbids `block_on` / `poll_once`. There are 11
`block_on` calls in non-test code:

- `crates/wafer-run-node/src/lib.rs:33, 75, 86, 93, 106, 109` — NAPI bindings
  bridge the async `Wafer` API with `self.rt.block_on(...)`.
- `crates/wafer-ffi/src/lib.rs:150, 166, 179, 276, 277` — C FFI bindings do the same.

Both files are sync boundaries to non-Rust runtimes (Node, C), so *some* bridging
is unavoidable. But the rule says no `block_on`, and the alternatives exist:

- **NAPI:** napi-rs 2 supports `#[napi]` async fns directly (returns JS `Promise`).
  Rewriting `WaferRuntime` methods as `async fn` removes every `block_on` and lets
  Node await naturally.
- **C FFI:** expose a callback-based API (`fn(*mut Ctx, on_done)`) and let the
  caller drive the loop, or spawn onto a dedicated runtime and signal completion
  via a fd / condvar. Either way, the runtime owns its tokio loop.

Test files also use `block_on` (~10 sites). Tests are pragmatically OK, but if
the rule is to be enforced everywhere, prefer `#[tokio::test]` over
`rt.block_on(async { … })`.

---

## P1 — Lock-poisoning unwraps on `std::sync::RwLock`

`parking_lot` is already a workspace dep (infallible locks), but the inspector
block uses `std::sync::RwLock` and unwraps on every access:

- `crates/wafer-block-inspector/src/lib.rs:75` — `self.policy.read().unwrap()`
- `crates/wafer-block-inspector/src/lib.rs:221` — `*self.policy.write().unwrap() = …`
- `crates/wafer-block-inspector/src/lib.rs:234` — same pattern

Any panic while holding the write lock poisons the policy and the inspector
will hard-crash on the next request, instead of degrading. Switch to
`parking_lot::RwLock` (drop the `.unwrap()` entirely) or, if std must be kept,
match on the poison and recover deliberately.

---

## P2 — Library error handling

Handbook (ch. 4): libraries return `Result<T, E>` with `thiserror`; binaries can
use `anyhow`. The split is **correct** here — only `wafer-cli` pulls `anyhow`,
every library crate has its own `error.rs` with `thiserror`. The leaks below are
isolated:

- `crates/wafer-block-http-listener/src/lib.rs:275` — bare `.unwrap()` in
  production code. Verify the invariant is provable and switch to `expect("…")`
  with a justification, or propagate the error.
- `crates/wafer-block-s3/src/lib.rs:53` — `.expect("wafer-run/s3: not initialized
  — call lifecycle(Init) first")`. The message is good; this is a programmer
  error, not a runtime error. Acceptable but a `Result` would let callers handle
  re-initialisation orderings.
- `crates/wafer-block-s3/src/service.rs:301` — `.expect("key is set")`. The
  invariant could be encoded in the type (e.g. accept a `&KeyedRequest` whose
  constructor enforces non-empty key) instead of asserted at runtime.

---

## P2 — Proc-macro should report errors via `syn::Error`, not `panic!`

`crates/wafer-block-macro/src/lib.rs` panics in ~13 places when parsing
`#[wafer_block(...)]` attributes (lines 69, 85, 93, 119, 122, 138, 147, 248,
256, 261, 338, 475 plus several `.expect()` on `path.get_ident()`).

Panics inside a proc macro surface as compiler ICEs without span information.
The idiomatic pattern is:

```rust
return Err(syn::Error::new(span, "unknown bool capability"));
// or, at the top level:
return syn::Error::new(span, "...").to_compile_error().into();
```

This gives block authors a red squiggle on the offending token instead of a
stack trace.

---

## P3 — Documentation hygiene

- No library crate sets `#![deny(missing_docs)]` or even `#![warn(missing_docs)]`.
  Crate-level doc comments exist on most (`wafer-block`, `wafer-core`,
  `wafer-run`) but `wafer-flow/src/lib.rs` and `wafer-sql-utils/src/lib.rs` have
  none. Adding `#![warn(missing_docs)]` at least to `wafer-block`, `wafer-core`,
  `wafer-run`, `wafer-flow`, and `wafer-sql-utils` would flag the public API
  gaps without breaking the build.
- Two un-tracked TODOs (handbook says every TODO links an issue):
  - `crates/wafer-core/src/clients/mod.rs:151` — "implement WASM sync call_block …"
  - `crates/wafer-cli/src/commands/publish.rs:37` — "stream via reqwest::Body::wrap_stream …"

---

## P3 — Lint suppressions: prefer `#[expect]` over `#[allow]`

19 `#[allow(...)]` sites, 0 `#[expect(...)]` sites. Handbook (ch. 2): `#[expect]`
becomes a warning when the lint *no longer fires*, which prevents stale
suppressions from drifting. On Rust 1.81+ (which the workspace uses), every new
suppression should be `#[expect(lint, reason = "…")]`.

---

## P3 — Cloning in `registry_loader`

`crates/wafer-run/src/registry_loader.rs` has ~30 `.clone()` calls on
`pkg.name` / `pkg.version` / `pkg.source` / `wasm_path` / `wt_path` across the
resolve path. These are all `String` / `PathBuf` clones in a function that
resolves the same package metadata once at startup, so the absolute cost is
small, but the pattern is what `clippy::redundant_clone` is trying to catch and
the lint is already set to `warn` workspace-wide (it just isn't firing because
the clones aren't *strictly* redundant per the lint's analysis).

Two cheap wins:

- Borrow `pkg` for the duration of resolution and only clone into the final
  `RegisteredPackage` struct.
- Switch the package-name interning to `Arc<str>` so re-use across the loader
  is free.

Not urgent. Worth a single follow-up PR.

---

## Strengths (don't regress these)

- **Workspace lints** are exactly the handbook-recommended set:
  `redundant_clone`, `large_enum_variant`, `needless_collect`,
  `uninlined_format_args`, `cloned_instead_of_copied`, `manual_let_else`,
  `unused_must_use` (`Cargo.toml:116-125`).
- **Library/binary error split is clean** — `anyhow` is confined to
  `wafer-cli`; every other crate uses `thiserror` via a local `error.rs`.
- **No unwraps in the hot paths of `wafer-run`, `wafer-block`, `wafer-flow`** —
  the unwraps that do exist are concentrated in tests, FFI boundaries, and
  proc-macro attribute parsing.
- **WASM-stub panics in `sdks/rust/*`** (`stream.rs`, `core_abi.rs`,
  `attachment.rs`) are appropriate: they document host-only imports with a
  clear panic message rather than silently failing on the host target.
- **Dynamic dispatch is used deliberately** — `Arc<dyn Block>`,
  `Box<dyn Fn(...)>` for routers/observability — at registry/extension
  boundaries, not in inner loops. Matches handbook ch. 6.

---

## Suggested order of work

1. **Fix the 6 clippy errors** (1 default-features + 5 `--all-features`). One
   PR, mechanical edits. Add the `--all-features` invocation to CI in the same PR.
2. **Replace `std::sync::RwLock` with `parking_lot::RwLock`** in
   `wafer-block-inspector`. Three-line change, removes three `.unwrap()` calls.
3. **Decide on the FFI/NAPI sync-bridge rule**: either (a) rewrite both
   bindings to expose async APIs natively and delete every production
   `block_on`, or (b) carve out a documented exception in `CLAUDE.md` scoped
   to "sync FFI boundary crates only". The current state contradicts the
   written rule.
4. **Proc-macro `panic!` → `syn::Error`** in `wafer-block-macro`. Mechanical
   but touches every attribute branch; do it as one PR.
5. **Doc & lint-hygiene pass**: add `#![warn(missing_docs)]` to the five library
   crates, convert existing `#[allow]` sites to `#[expect]` with reasons, link
   the two stray TODOs to issues.
6. **`registry_loader` clone audit** — last, lowest impact.
