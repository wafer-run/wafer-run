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

## P3 — Documentation hygiene (deferred)

- No library crate sets `#![deny(missing_docs)]` or even `#![warn(missing_docs)]`.
  Crate-level doc comments exist on most (`wafer-block`, `wafer-core`,
  `wafer-run`) but `wafer-flow/src/lib.rs` and `wafer-sql-utils/src/lib.rs` have
  none. Adding the lint at `warn` would not break CI on its own — but the
  workspace runs clippy with `-D warnings`, which upgrades every missing-doc
  emission to an error. Adding `#![warn(missing_docs)]` to the five library
  crates therefore needs to be paired with a wave of doc additions across the
  public API surface; that's a substantially larger task and out of scope for
  this review's follow-ups. Leaving as a future PR.
- Two un-tracked TODOs (handbook says every TODO links an issue):
  - `crates/wafer-core/src/clients/mod.rs:151` — "implement WASM sync call_block …"
  - `crates/wafer-cli/src/commands/publish.rs:37` — "stream via reqwest::Body::wrap_stream …"

  Linkage deferred — needs issue numbers from the project tracker.

---

## P3 — Lint suppressions: prefer `#[expect]` over `#[allow]` (FIXED)

Originally 19 `#[allow(...)]` sites with 0 `#[expect(...)]`. After the hygiene
commit on `chore/clippy-clean`, 17 sites converted to
`#[expect(lint, reason = "…")]`. Two sites stay on `#[allow(...)]` (with the
same reason annotation) because the lint state depends on which target is
being compiled — `#[expect]` would be unfulfilled under `cargo clippy
--all-targets`:

- `sdks/rust/src/stream.rs` — `error_code_from_ordinal` (dead in lib, used in tests)
- `wafer-cli/src/wafer_toml.rs` — `WaferToml::remove_dependency` (same pattern)

Two `#[allow(dead_code)]` sites were dropped entirely because the code is
actually reachable under `--all-targets`:

- `wafer-cli/src/detect.rs` — `detect_language`
- `wafer-cli/src/manifest.rs` — `Manifest::load`

---

## P3 — Cloning in `registry_loader` (re-assessed: no fix needed)

The original finding overcounted. A re-read of
`crates/wafer-run/src/registry_loader.rs` shows the `.clone()` calls on
`pkg.name` / `pkg.version` / `wasm_path` / `wt_path` are almost entirely in
error-construction paths — building owned-`String` `LockLoaderError` variants.
Errors are cold by definition; clones there are correct.

The one happy-path clone is `self.register_block(pkg.name.clone(), …)` in
`load_lockfile_parsed`, which feeds an owned name to `register_block`. That's
a one-clone-per-block-at-startup cost — not measurable.

`redundant_clone` (workspace-warn) and the SSA analysis behind it already
agree: no clone is dropping its target without using it. Closing this item.

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

## Suggested order of work — status

1. ✅ **Fix the 6 clippy errors** — landed on `chore/clippy-clean` as the P0
   commit. `cargo clippy --workspace --all-targets --locked -- -D warnings`
   exits 0. `--all-features` deliberately not pursued (architecturally invalid
   — see P0 section).
2. ✅ **Replace `std::sync::RwLock` with `parking_lot::RwLock`** in
   `wafer-block-inspector`. Done — three `.unwrap()` calls removed.
3. ✅ **Sync-bridge rule (option a)** — rewrote both bindings:
   - `wafer-run-node` — all `#[napi]` methods are now `async fn` returning JS
     `Promise<T>`; `block_on` calls eliminated.
   - `wafer-ffi` — async ops (resolve/start/stop/run) are callback-based;
     `block_on` replaced with `rt.spawn`. Go bindings updated to wrap the
     callback C ABI back into a synchronous Go API surface via `cgo.Handle` +
     channels. Both `wafer.h` copies (Rust crate + Go module) updated in lockstep.
4. ✅ **Proc-macro `panic!` → `syn::Error`** in `wafer-block-macro`. Done —
   ~22 attribute-time panic sites routed through `syn::Result` and emitted via
   `to_compile_error()`. Block authors now get spanned diagnostics.
5. ◐ **Doc & lint-hygiene pass** — partially done:
   - ✅ Lint-suppression hygiene: 17 `#[allow]` → `#[expect]` with reasons; 2
     `#[allow]` dropped; 2 stay on `#[allow]` because `#[expect]` would be
     unfulfilled under `--all-targets`.
   - ❌ `#![warn(missing_docs)]` — deferred (incompatible with workspace
     `-D warnings` without a wider doc-coverage pass).
   - ❌ TODO issue linkage — deferred (needs tracker IDs).
6. ✖ **`registry_loader` clone audit** — closed without code change. Original
   finding overcounted; the clones are appropriate (see re-assessment above).
