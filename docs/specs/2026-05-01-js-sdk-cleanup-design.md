# JS SDK cleanup: retire the half-implemented JavaScript/TypeScript block runtime

**Date:** 2026-05-01
**Status:** Proposed
**Initiative:** Hardening Spec 3 (Developer Experience), sub-spec C.
**Predecessors:** Spec 1 (PR #3), Spec 2A (PR #4), Spec 2B (PR #5), Spec 3A (PR #28), Spec 3B (PR #32).
**Successor:** None — closes the hardening initiative.

## Goal

Remove the JavaScript/TypeScript block subsystem from `wafer-run`. It is half-implemented (host imports throw at runtime), undocumented (no README mentions, no examples), untested (no test exercises the TS scaffold or build path), and unused (no consumer in this repo or known downstream). Closing the initiative honestly is preferable to shipping a partial runtime that future consumers would have to work around.

## Context

Spec 3 (developer experience) was originally split into three sub-specs:

- **3A** — documentation (MERGED, PR #28).
- **3B** — `wafer dev` CLI (MERGED, PR #32).
- **3C** — JS SDK host runtime: make `callBlock` / `log` / `isCancelled` actually work in `sdks/js`.

3C's framing assumed the JS block runtime was substantially complete and only the host-import seam needed wiring. Investigation during brainstorming on 2026-05-01 found that:

1. The block-authoring API in `sdks/js/src/{index,types,host}.ts` is real but the three host functions are throw-stubs labelled *"only available inside a compiled WASM block linked to the WAFER runtime."*
2. The build pipeline (`crates/wafer-cli/src/build.rs::build_typescript`) is real and end-to-end: esbuild bundles `src/index.ts` into `runtime/bundle.js`, then `cargo build --target wasm32-wasip1 --release` compiles a `boa_engine`-based runtime crate (templated by `crates/wafer-cli/src/scaffold.rs::RUNTIME_LIB_RS`, ~200 lines) that evaluates the bundle on first call.
3. The host imports were never wired: `boa_engine` is embedded but `callBlock`/`log`/`isCancelled` are not registered as Rust closures on the boa `Context`. A TS block that limits itself to request/response would technically run; any block that calls a host function panics.
4. There are no TypeScript examples in `examples/`, no mention of TypeScript in `README.md` / `CONTRIBUTING.md` / `docs/`, no test in `crates/wafer-cli/tests/` that exercises `Lang::TypeScript`, and `@wafer-run/sdk` was never published to npm (root `package.json` is `"private": true`; the package has no `publishConfig`).

Building out 3C as originally framed would mean: designing host-import bindings for boa, wiring them through the per-block runtime template, polyfilling enough Web/Node API surface for realistic blocks (the existing template ships only a `TextEncoder` polyfill), writing tests, writing docs, and shipping `@wafer-run/sdk` to npm. Several weeks of work with no consumer driving requirements. Workspace rule is "no half-finished implementations" — the principled move is to remove the half-product and revisit when there is a real ask.

## Non-goals

- **Designing a future JS block runtime.** When a consumer surfaces, that brainstorm starts fresh. This spec does not pre-commit to boa, Javy, ComponentizeJS, QuickJS, or any other engine.
- **Deprecation period or compat shims.** `@wafer-run/sdk` is unpublished; `Lang::TypeScript` is undocumented. Hard remove.
- **Tracking issue creation.** Optional follow-up the maintainer may open separately. Git history is sufficient for revival.
- **Touching `packages/wafer-client-js`.** That is the *HTTP client* SDK — independent, working, used. Out of scope.
- **Touching `crates/wafer-run-node`.** That is the napi-rs addon for *embedding* the runtime in Node — different direction, working, out of scope.

## Removal manifest

The PR is mostly deletions and a regenerated lockfile.

| # | Path | What goes |
|---|---|---|
| 1 | `sdks/js/` | entire directory (3 source files, 149 lines) plus `package.json`, `tsconfig.json`, `node_modules/` (gitignored), `dist/` (gitignored) |
| 2 | `package.json` (root) | drop `"sdks/js"` from `workspaces` |
| 3 | `package-lock.json` | regenerate via `npm install` after (1)+(2) |
| 4 | `crates/wafer-cli/src/scaffold.rs` | `Lang::TypeScript` arms in `scaffold()` (lines 35-39 and 53-60); entire `scaffold_typescript()` function (~185-296); `RUNTIME_LIB_RS` const + `TEXT_ENCODER_POLYFILL` const + helpers `with_context` / `pack_ptr_len` / `string_to_packed` (~298-470) — verify each helper has no other caller before removing. |
| 5 | `crates/wafer-cli/src/detect.rs` | `Lang::TypeScript` enum variant; `"typescript" \| "ts"` parse arm (~line 19); `"Supported: rust, go, typescript"` error message → `"Supported: rust, go"`; `package.json` detection branch (~lines 40-42); doc-comment line referencing TypeScript (~line 32) |
| 6 | `crates/wafer-cli/src/build.rs` | `Lang::TypeScript` arm in `build()` (line 52); entire `build_typescript()` function; `find_wasm_in_dir()` if and only if it has no other callers (verify by grep within the crate). |
| 7 | `crates/wafer-cli/src/validate.rs` | `wasi_snapshot_preview1::poll_oneoff` stub (~lines 181-199) and `sched_yield` stub (~lines 201-209). Both are comment-tagged "Required by boa_engine-compiled WASM modules (JS/TS blocks)" and have no other purpose. |
| 8 | CLI surface | any `clap` arg enum that lists `ts` / `typescript` as a `--lang` choice (verify in `crates/wafer-cli/src/main.rs` and `commands/`); CLI help text; any `wafer new --lang ts` examples in tests or docs |

Estimated diff: roughly 600 lines removed, 0 added (excluding lockfile churn).

## Verification plan

- `cargo test --workspace --exclude wafer-run && cargo test -p wafer-run --lib` — full workspace baseline still passes. (CI-mirror command from workspace conventions; the plain `cargo test --workspace` form trips the gitignored `echo_block.wasm` integration test.)
- `cargo clippy --workspace --all-targets -- -D warnings` — no orphaned imports or dead-code warnings remain after the deletions.
- `cargo +nightly fmt --all` — required before push (CI Format & Lint job runs nightly rustfmt).
- Manual smoke test:
  - `wafer new --lang rust org/foo` → succeeds.
  - `wafer new --lang go org/foo` → succeeds.
  - `wafer new --lang ts org/foo` → fails with the updated `"Supported: rust, go"` error.
  - `wafer build` in an existing Rust block project → succeeds.
- `npm install` at repo root succeeds with `sdks/js` removed from `workspaces`.
- Final sanity sweep: `git grep -i 'typescript\|sdks/js\|@wafer-run/sdk\|boa_engine'` returns no hits in tracked files.

## Spec bookkeeping

- This spec: `wafer-run/docs/specs/2026-05-01-js-sdk-cleanup-design.md`.
- Implementation plan (next): `wafer-run/docs/plans/2026-05-01-js-sdk-cleanup.md`.
- Worktree for execution: `/workspace/wafer-run-js-sdk-cleanup/` on branch `feat/js-sdk-cleanup` (already created off `origin/main` with the gitignored `echo_block.wasm` copied in to satisfy the pre-commit hook).
- Hardening state memory (`wafer-run-hardening-state.md`) updated on merge: 3C marked done as cleanup, initiative closed.

## Risks

- **Lockfile churn.** Regenerating `package-lock.json` will touch many lines unrelated to the SDK removal. Acceptable; CI will validate. The regeneration is a single `npm install` and the diff is mechanical.
- **Hidden external consumer.** Searched all tracked files in `wafer-run`; cross-checked `examples/`, `docs/`, `README.md`. The `@wafer-run/sdk` package is unpublished, so an external consumer would have had to depend on it via a git URL — possible but unlikely. Flag in PR description so the maintainer can confirm before merge.
- **Future revival cost.** When (if) a JS block runtime becomes a real requirement, design and implementation start from scratch rather than picking up where this code stopped. Acceptable trade — the existing code likely would not match the eventual real design (it pre-commits to boa-per-block, no Web API surface, no streaming, no host-side cancellation, no async). Revival from a clean slate will produce a better runtime than revival from this skeleton.
