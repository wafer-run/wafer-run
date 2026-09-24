# Contributing to wafer-run

Welcome! wafer-run is pre-1.0 and PR-driven. Bug reports, feature requests, and design questions all go through [GitHub issues](https://github.com/wafer-run/wafer-run/issues). Specs and plans live in [`docs/specs/`](./docs/specs/) and [`docs/plans/`](./docs/plans/).

If you're new to wafer as a *user*, read [wafer.run/docs/core-concepts](https://wafer.run/docs/core-concepts) first — this file assumes you've seen the conceptual model.

---

## Toolchain

- **Rust stable** — install via [rustup](https://rustup.rs). `rust-toolchain.toml` pins the exact version (and its `clippy`, `rustfmt` and wasm targets); rustup installs it on first use.
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

`./scripts/check.sh` is the single definition of "green" — CI's jobs
(`.github/workflows/ci-jobs.yml`) invoke its named steps, and running it
with no arguments runs the full sequence locally before a PR.

Its steps are `fixtures fmt clippy test postgres wasm audit`; run any of
them by name. The test step is what CI runs:

```
./scripts/check.sh fixtures test
```

`test` is `cargo test --workspace` plus the two `allow-private-network`
SSRF e2e suites. `cargo test --workspace` compiles `wafer-run`'s integration
tests, which need the WASM fixtures — hence `fixtures` first (see the gotcha
below). The `postgres` step runs the database conformance suite against a
live server named by `WAFER_CONFORMANCE_POSTGRES_URL`; the header of
`scripts/check.sh` shows how to start one with Docker.

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
  wafer-test-support/    Test runtime helper (WaferBuilder)
  wafer-sql-utils/       Type-safe SQL builders (use these, no raw SQL)
  wafer-ffi/             FFI bindings
  wafer-run-node/        Node-side host integration

examples/                Runnable demos. See examples/README.md.
sdks/rust/               wafer-sdk for guest WASM blocks (consumes #[wafer_block])
packages/wafer-client-js/JS/TS client. See its README.
docs/specs/              Design specs (one per initiative)
docs/plans/              Implementation plans (one per spec)
go/                      Go bindings.
```

## Code style

- **Format with nightly rustfmt.** `rustfmt.toml` uses nightly-only options (`imports_granularity`, `group_imports`) that stable rustfmt ignores. CI's Format & Lint runs `cargo +nightly fmt --all -- --check`, and the pre-commit hook runs `cargo +nightly fmt --all`, so install it once: `rustup toolchain install nightly --component rustfmt`.
- **Clippy clean:** `./scripts/check.sh clippy` (`cargo clippy --workspace --all-targets -- -D warnings`, the CI command; the pre-commit hook runs the same lint set with `--fix`). `--all-targets` compiles the integration tests, so it needs the fixtures — see the gotcha above.
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

3. The pre-commit hook runs `cargo +nightly fmt` + `cargo clippy --all-targets --fix`. Don't bypass with `--no-verify`. If the hook fails, fix the underlying issue.

4. Before pushing:
   ```
   cargo +nightly fmt --all
   ./scripts/check.sh
   ```
   With no arguments `check.sh` runs every step, skipping `postgres` (loudly) when `WAFER_CONFORMANCE_POSTGRES_URL` is unset.

5. Open the PR. CI must pass before merge: the `ci / ci-ok` check is green only when every job in `.github/workflows/ci-jobs.yml` succeeded. A new CI job goes in that file and in `ci-ok`'s `needs` (`scripts/lint-workflows.sh` fails the build otherwise), and every action is pinned by full commit SHA. Squash-merge is the default.

## Security advisories

The `audit` job (`scripts/check.sh audit`, part of `ci / ci-ok`) runs
`cargo audit` against the RustSec database as it is **at run time**, not as
it was when a branch was cut. When an advisory is published against a crate
already in `Cargo.lock`, every open PR and every push to `main` turns red at
once, including PRs that touch nothing near that crate, until someone
handles the advisory on `main`. The weekly scheduled run of
`.github/workflows/audit.yml` reports it even when no PR is open.

To handle one, in a PR of its own:

1. Run `cargo audit` locally to see the advisory, the affected versions, and
   the `patched` range. `cargo tree --locked -i <crate>@<version>` shows
   who pulls the crate in.
2. If a patched version is semver-compatible, update just that crate
   (`cargo update -p <crate>@<old> --precise <new>`) and commit
   `Cargo.lock`. This is the normal case.
3. If the fix needs a newer major of a direct dependency, bump it in the
   owning crate's `Cargo.toml` and fix what breaks.
4. Only when no fix is reachable (no patched release, or a transitive
   dependency pins the vulnerable line), add the id to
   `.cargo/audit.toml`'s `ignore` list: on its own line, directly below a
   comment block with a `REASON:` line (why it cannot be fixed now and
   where the crate comes from, including whether it reaches a production
   artifact) and a `REMOVE WHEN:` line (the concrete condition that retires
   the ignore). `scripts/lint-workflows.sh` fails CI when either line is
   missing. Remove the entry as soon as its condition is met.

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
