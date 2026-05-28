# Contributing to wafer-run

Welcome! wafer-run is pre-1.0 and PR-driven. Bug reports, feature requests, and design questions all go through [GitHub issues](https://github.com/wafer-run/wafer-run/issues). Specs and plans live in [`docs/specs/`](./docs/specs/) and [`docs/plans/`](./docs/plans/).

If you're new to wafer as a *user*, read [wafer.run/docs/core-concepts](https://wafer.run/docs/core-concepts) first — this file assumes you've seen the conceptual model.

---

## Toolchain

- **Rust stable** — install via [rustup](https://rustup.rs). No `rust-toolchain.toml`; the latest stable works.
- **Rust nightly** — required for `cargo +nightly fmt --all` (CI's Format & Lint job runs nightly rustfmt to enforce `imports_granularity = "Crate"` and `group_imports = "StdExternalCrate"` from `rustfmt.toml`; stable rustfmt silently ignores those rules).
  ```
  rustup toolchain install nightly --component rustfmt
  ```
- **`wasm32-wasip1` target** — for guest WASM block development.
  ```
  rustup target add wasm32-wasip1
  ```
- **Node 20+** — for `packages/wafer-client-js`. Skip if you're not touching the JS client.

## Build & test

The default test command **mirrors CI**:

```
cargo test --workspace --exclude wafer-run && cargo test -p wafer-run --lib
```

Don't use `cargo test --workspace` directly — it will compile `wafer-run`'s integration tests, including `wasmi_block_test.rs`, which depends on a WASM testdata fixture. See the gotcha below.

For a quick build sanity check:

```
cargo build --workspace
```

This builds everything except `examples/wasmi-block` (which is a standalone workspace — see its README).

### Gotcha — wasm test fixtures

Three test fixtures are `.gitignore`d (the `*.wasm` rule) and not tracked:

- `crates/wafer-run/testdata/echo_block.wasm` — consumed by `wasmi_block_test.rs` via `include_bytes!` (compile-error if missing).
- `crates/wafer-run/tests/attachment_dispatch/target/wasm32-wasip1/release/attachment_dispatch_guest.wasm` — consumed by `attachment_e2e_wasmi.rs` at runtime.
- `crates/wafer-run/tests/dispatch_guest/target/wasm32-wasip1/release/dispatch_guest.wasm` — consumed by `dispatch_streaming.rs` at runtime.

A fresh clone will lack all three; the pre-commit hook will fail clippy without `echo_block.wasm`, and `cargo test --workspace` will fail individual tests without the other two.

**Fix:** run once after cloning:

```
./scripts/build-fixtures.sh
```

The pre-commit hook also calls this script, so a fresh worktree's first commit will trigger the build automatically (~30–60s one-time cost; subsequent commits skip in <100ms).

The script is idempotent and safe to re-run.

## Repo layout

```
crates/
  wafer-run/             Runtime entry point (see crates/wafer-run/README.md)
  wafer-block/           Shared types crate (BlockInfo, ConfigVar, ...)
  wafer-block-macro/     #[wafer_block] proc macro
  wafer-block-*/         First-party blocks (sqlite, postgres, http-listener, ...)
  wafer-cli/             CLI binary (wafer search/info/install/publish)
  wafer-flow/            Flow composition
  wafer-flow-http-server/HTTP server hosting flows
  wafer-core/            Runtime core
  wafer-test-support/    Test fakes (FakeDb, FakeCrypto, WaferBuilder)
  wafer-sql-utils/       Type-safe SQL builders (use these, no raw SQL)
  wafer-ffi/             FFI bindings
  wafer-run-node/        Node-side host integration

examples/                Runnable demos. See examples/README.md.
sdks/rust/               wafer-sdk for guest WASM blocks (consumes #[wafer_block])
packages/wafer-client-js/JS/TS client. See its README.
registry/                Block manifests served by the registry.
docs/specs/              Design specs (one per initiative)
docs/plans/              Implementation plans (one per spec)
common/                  Shared resources. See its README.
go/                      Go bindings.
```

## Code style

- **Format with stable for local, nightly before push.** The pre-commit hook runs `cargo fmt` (stable). CI's Format & Lint runs `cargo +nightly fmt --all -- --check`. **Run `cargo +nightly fmt --all` before every push** or CI fails.
- **Clippy clean:** `cargo clippy --workspace -- -D warnings` (CI command). Locally, `cargo clippy --all-targets` is stricter and is what the pre-commit hook runs — see the testdata gotcha above for what `--all-targets` pulls in.
- **No sync bridges.** No `poll_once`, no `block_on`. If something is async, callers must remain async. (See `CLAUDE.md`.)
- **No raw SQL in block code.** Use `wafer-sql-utils` builders (`query::*`, `aggregate::*`, `upsert::*`, `ddl::*`, `introspect::*`). If a builder is missing for what you need, add it to `wafer-sql-utils` — don't fall back to `exec_raw`/`query_raw`. Exceptions: the admin SQL explorer (user-typed query), migration-file runners, and test-fixture setup.
- **No hardcoded domain values.** Block-specific values come from `ConfigVar` declared on the block's `BlockInfo::config_keys`. (See `CLAUDE.md`.)
- **Fix at root cause.** No code smells, no compat shims, no quick fixes. If the right fix touches many files, touch them.

## Branch + PR workflow

1. Branch from `main`:
   ```
   git checkout main && git pull --ff-only
   git checkout -b feat/<topic>
   ```
   ⚠️ **Do NOT** `git checkout -b feat/<topic> origin/main` — that sets upstream to `origin/main` and a later `git push` pushes to main directly. Plain `-b feat/<topic>` (no second argument) is correct.

2. Use conventional commit prefixes: `feat:`, `fix:`, `chore:`, `docs:`, `refactor:`, `test:`, `ci:`. The PR title mirrors the leading commit's prefix.

3. The pre-commit hook runs `cargo fmt` + `cargo clippy --all-targets --fix`. Don't bypass with `--no-verify`. If the hook fails, fix the underlying issue.

4. Before pushing:
   ```
   cargo +nightly fmt --all
   cargo clippy --workspace -- -D warnings
   cargo test --workspace --exclude wafer-run && cargo test -p wafer-run --lib
   ```

5. Open the PR. CI must pass before merge. Squash-merge is the default.

## Worktrees for parallel work

When you have multiple in-flight branches and want to avoid context-switching the main checkout, use a worktree:

```
git worktree add ../wafer-run-<topic> -b feat/<topic>
```

Each spec in the hardening initiative has used a sibling worktree at `/workspace/wafer-run-<topic>/`. After merging the branch, remove the worktree:

```
git worktree remove ../wafer-run-<topic>
```

## Where to ask

- **Bugs & feature requests:** [issues](https://github.com/wafer-run/wafer-run/issues).
- **Design questions:** read [`docs/specs/`](./docs/specs/) for prior decisions, then open an issue.
- **User docs:** [wafer.run/docs](https://wafer.run/docs/quick-start).
