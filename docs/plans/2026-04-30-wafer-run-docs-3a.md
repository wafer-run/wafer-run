# wafer-run Spec 3A — Repo Documentation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the repo-level documentation files identified in Spec 3A: `LICENSE`, `README.md`, `CONTRIBUTING.md`, `examples/README.md`, and a uniform README in each of the 6 example directories. Branch + PR per workspace convention.

**Architecture:** Documentation-only PR. Ten new markdown/text files, no edits to existing files except the spec/plan additions. README is a billboard funneling to `wafer.run/docs` and `examples/hello-world`; CONTRIBUTING is the dev-onboarding companion (toolchain, build, test, repo layout, code style, PR workflow). Per-example READMEs share a uniform 4-section skeleton.

**Tech Stack:** Markdown, plain text. Verification uses `grep`, `cargo build --workspace`, and the CI-mirror test command (`cargo test --workspace --exclude wafer-run && cargo test -p wafer-run --lib`).

**Spec:** `docs/specs/2026-04-30-wafer-run-docs-3a-design.md`

---

## File map

Files this plan creates (all in `/home/joris/Programs/suppers-ai/workspace/wafer-run/`):

- `LICENSE` — verbatim MIT, copyright "2026 wafer.run contributors". Backs the existing `Cargo.toml` `license = "MIT"` declaration.
- `README.md` — billboard for GitHub visitors. Sections: title + tagline, why wafer (5 bullets), quick taste (illustrative snippet), get started (3 links), repo layout, license, status.
- `CONTRIBUTING.md` — dev onboarding. Sections: welcome, toolchain, build & test (with the `echo_block.wasm` gotcha and CI-mirror command), repo layout, code style, branch + PR workflow, worktrees, where to ask.
- `examples/README.md` — table-of-examples index.
- `examples/hello-world/README.md` — "smallest end-to-end" canonical first run.
- `examples/api-server/README.md` — JSON REST shape.
- `examples/middleware-chain/README.md` — composed middleware.
- `examples/multi-flow/README.md` — multiple flows in one binary.
- `examples/static-site/README.md` — `web` block serving files.
- `examples/wasmi-block/README.md` — authoring a guest WASM block (different shape: cdylib, standalone workspace, `wasm32-wasip1` target).

No worktree is created — doc-only work doesn't need isolation, and a regular feature branch is sufficient.

---

### Task 1: Create feature branch in wafer-run

**Files:** none

- [ ] **Step 1: Verify clean main and create branch**

```bash
cd /home/joris/Programs/suppers-ai/workspace/wafer-run
git remote -v                       # expect: origin → github.com/wafer-run/wafer-run
git checkout main
git pull --ff-only
git status -s                       # should show only the new spec at docs/specs/2026-04-30-wafer-run-docs-3a-design.md and this plan
git checkout -b feat/docs-3a
git status
git branch -vv | grep feat/docs-3a
```

Expected:
- After `checkout main`: branch is main; pull is up-to-date.
- After `git status -s`: shows `?? docs/specs/2026-04-30-wafer-run-docs-3a-design.md` and `?? docs/plans/2026-04-30-wafer-run-docs-3a.md` (the spec + this plan, untracked).
- After `git checkout -b feat/docs-3a`: `On branch feat/docs-3a`. `git branch -vv` shows the new branch with NO upstream tracking (no `[origin/...]` after the branch name).

⚠️ **Do NOT use `git checkout -b feat/docs-3a origin/main`** — that sets upstream to origin/main and a later push pushes to main directly. Plain `-b feat/docs-3a` (no second argument) is correct.

- [ ] **Step 2: Stage the spec + plan as the first commit**

```bash
git add docs/specs/2026-04-30-wafer-run-docs-3a-design.md docs/plans/2026-04-30-wafer-run-docs-3a.md
git commit -m "docs(spec): add Spec 3A design + plan for repo documentation"
git log --oneline -1
```

Expected: one commit on `feat/docs-3a`, message `docs(spec): add Spec 3A design + plan for repo documentation`.

---

### Task 2: Add LICENSE

**Files:**
- Create: `/home/joris/Programs/suppers-ai/workspace/wafer-run/LICENSE`

- [ ] **Step 1: Write the MIT license**

Create `LICENSE` with verbatim MIT text:

```
MIT License

Copyright (c) 2026 wafer.run contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

- [ ] **Step 2: Verify and commit**

```bash
test -f LICENSE && head -3 LICENSE
git add LICENSE
git commit -m "docs: add MIT LICENSE file"
```

Expected: file exists, first three lines are the MIT header.

---

### Task 3: Add README.md

**Files:**
- Create: `/home/joris/Programs/suppers-ai/workspace/wafer-run/README.md`

- [ ] **Step 1: Verify the existing crate-level README, so the root README doesn't contradict it**

```bash
cat crates/wafer-run/README.md
```

The root README must be consistent with whatever's in `crates/wafer-run/README.md`. Note any references to the crate-level spec (`crates/wafer-run/spec/WAFER_SPEC.md`) so the root README can link to it if appropriate.

- [ ] **Step 2: Write the root README**

Create `README.md`:

````markdown
# wafer.run

**WAFER** — *WebAssembly Architecture for Flow Execution & Routing*. A wafer-thin runtime for tools, apps, and services. One binary, composable WASM blocks, declarative flows.

[**→ Documentation**](https://wafer.run/docs/quick-start) · [**→ Run an example**](./examples/hello-world) · [**→ Contributing**](./CONTRIBUTING.md)

---

## Why wafer

- **Single binary.** Drop `wafer-run` (or your wafer-built binary) on a host; no runtime to install.
- **WASM blocks.** Sandboxed, language-agnostic guest code via `wasmi` with resumable async host calls. See [creating a block](https://wafer.run/docs/creating-a-block).
- **Composable flows.** Declarative flow files describe how blocks chain into request pipelines. See [waferflow](https://wafer.run/docs/waferflow).
- **Package registry.** `wafer search`, `wafer install`, `wafer publish` — see [the registry](https://wafer.run/docs/registry).
- **Secure by default.** Per-block capabilities enforced at runtime (WRAP). See [block capabilities](https://wafer.run/docs/block-capabilities).

## Quick taste

```rust
use wafer_run::Wafer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let wafer = Wafer::builder()
        .register_block("wafer-run/hello", HelloBlock::default())?
        .build()?;

    wafer.start().await?;          // boot all blocks
    wafer.call_block("wafer-run/hello", "greet", &[]).await?;
    Ok(())
}
```

The full runnable version lives at [`examples/hello-world/`](./examples/hello-world).

## Get started

| If you want to … | Go to |
| --- | --- |
| Read the docs            | [wafer.run/docs/quick-start](https://wafer.run/docs/quick-start) |
| Clone and run an example | [`examples/hello-world`](./examples/hello-world) |
| Hack on wafer-run itself | [`CONTRIBUTING.md`](./CONTRIBUTING.md) |

## Repo layout

```
crates/        Rust crates (runtime, blocks, CLI, SDK)
examples/      Runnable demos — start with hello-world
sdks/          Guest SDKs (Rust today)
packages/      JS/TS client (wafer-client-js)
registry/      Block manifests for the wafer-run registry
docs/          Specs (docs/specs/) and plans (docs/plans/)
common/        Shared resources (see common/README.md)
go/            Go bindings
```

## License

MIT — see [`LICENSE`](./LICENSE).

## Status

Pre-1.0. APIs and schemas are still moving; breaking changes land without deprecation cycles. Registry is currently private (flips public when wafer-run crates publish to crates.io).
````

- [ ] **Step 3: Spot-check the snippet against the current `wafer-run` API**

The Quick-taste snippet uses:
- `Wafer::builder()` returning a builder
- `.register_block(name, block)?` (fallible)
- `.build()?` (fallible)
- `wafer.start().await?`
- `wafer.call_block(name, action, &[]).await?`

Verify these signatures are roughly right by grepping:

```bash
grep -nE 'pub fn builder\(' crates/wafer-run/src/lib.rs
grep -nE 'pub.*fn register_block' crates/wafer-run/src/*.rs
grep -nE 'pub.*async fn start' crates/wafer-run/src/*.rs
grep -nE 'pub.*async fn call_block' crates/wafer-run/src/*.rs
```

If any signature has drifted (e.g., `register_block` is not on the builder, or `call_block` takes a different argument shape), update the snippet to match. The snippet is illustrative, not compiled — exact-API fidelity is not required, but the shape (builder pattern, named blocks, async start/call) must match reality.

- [ ] **Step 4: Verify and commit**

```bash
wc -l README.md                                # expect 60-110 lines
grep -c 'wafer.run/docs' README.md             # expect 5+ (5 doc links)
grep -c 'examples/hello-world' README.md       # expect 2+
grep -c 'CONTRIBUTING' README.md               # expect 2+
git add README.md
git commit -m "docs: add root README"
```

---

### Task 4: Add CONTRIBUTING.md

**Files:**
- Create: `/home/joris/Programs/suppers-ai/workspace/wafer-run/CONTRIBUTING.md`

- [ ] **Step 1: Confirm CI test commands and rustfmt setup**

```bash
grep -E 'cargo|rustfmt' .github/workflows/ci.yml
cat rustfmt.toml
```

The CI test command (verified at spec time) is:
```
cargo test --workspace --exclude wafer-run && cargo test -p wafer-run --lib
```

`rustfmt.toml` contains `imports_granularity = Crate` and `group_imports = StdExternalCrate` (nightly-only rules). If either has changed since the spec, update CONTRIBUTING accordingly.

- [ ] **Step 2: Write CONTRIBUTING.md**

Create `CONTRIBUTING.md`:

````markdown
# Contributing to wafer-run

Welcome! wafer-run is pre-1.0 and PR-driven. Bug reports, feature requests, and design questions all go through [GitHub issues](https://github.com/wafer-run/wafer-run/issues). Specs and plans live in [`docs/specs/`](./docs/specs/) and [`docs/plans/`](./docs/plans/).

If you're new to wafer as a *user*, read [wafer.run/docs/core-concepts](https://wafer.run/docs/core-concepts) first — this file assumes you've seen the conceptual model.

---

## Toolchain

- **Rust stable** — install via [rustup](https://rustup.rs). No `rust-toolchain.toml`; the latest stable works.
- **Rust nightly** — required for `cargo +nightly fmt --all` (CI's Format & Lint job runs nightly rustfmt to enforce `imports_granularity = Crate` and `group_imports = StdExternalCrate` from `rustfmt.toml`; stable rustfmt silently ignores those rules).
  ```
  rustup toolchain install nightly
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

### Gotcha — `crates/wafer-run/testdata/echo_block.wasm`

This fixture is `.gitignore`d (the `*.wasm` rule) and is NOT tracked. A fresh clone will lack it. `wasmi_block_test.rs` `include_bytes!`s it, so `cargo test -p wafer-run --tests` and `cargo clippy --all-targets` will fail with a missing-file error.

**Two workarounds:**

1. **Use the CI-mirror test command above** — it runs only `wafer-run`'s `--lib` tests, sidestepping the integration test.
2. **Generate the fixture** (needed if you're modifying `wafer-run`'s integration tests):
   ```
   (cd examples/wasmi-block && cargo build --release --target wasm32-wasip1)
   cp examples/wasmi-block/target/wasm32-wasip1/release/wafer_example_wasmi_echo.wasm \
      crates/wafer-run/testdata/echo_block.wasm
   ```
   The pre-commit hook runs `cargo clippy --all-targets --fix`, so without this fixture the hook will fail. Generate it once after cloning and the file persists in your worktree.

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
````

- [ ] **Step 3: Verify and commit**

```bash
wc -l CONTRIBUTING.md                                          # expect 150-300 lines
grep -c 'wafer.run/docs' CONTRIBUTING.md                        # expect 3+
grep -c 'cargo +nightly fmt' CONTRIBUTING.md                    # expect 2+
grep -c 'echo_block.wasm' CONTRIBUTING.md                       # expect 2+
grep -c 'feat/<topic> origin/main' CONTRIBUTING.md              # expect 1 (the footgun warning)
git add CONTRIBUTING.md
git commit -m "docs: add CONTRIBUTING guide"
```

---

### Task 5: Add examples/README.md

**Files:**
- Create: `/home/joris/Programs/suppers-ai/workspace/wafer-run/examples/README.md`

- [ ] **Step 1: Verify each example's binary name (used in `cargo run -p <name>`)**

```bash
for d in examples/*/; do
  echo "=== $d ==="
  grep -E '^name = ' "$d/Cargo.toml" | head -1
done
```

Expected names (verified at spec time):
- `examples/api-server/` → `api-server`
- `examples/hello-world/` → `hello-world`
- `examples/middleware-chain/` → `middleware-chain`
- `examples/multi-flow/` → `multi-flow`
- `examples/static-site/` → `static-site`
- `examples/wasmi-block/` → `wafer-example-wasmi-echo` (different — it's a cdylib in a standalone workspace)

If any name in the table below differs, update before writing.

- [ ] **Step 2: Write `examples/README.md`**

Create `examples/README.md`:

````markdown
# wafer-run examples

Each subdirectory is a self-contained, runnable example. New to wafer? Start with [**hello-world**](./hello-world).

| Example | What it demonstrates | Run |
|---|---|---|
| [`hello-world`](./hello-world)         | Smallest possible wafer + HTTP server | `cargo run -p hello-world`         |
| [`api-server`](./api-server)           | Wafer behind a JSON REST API          | `cargo run -p api-server`          |
| [`static-site`](./static-site)         | Wafer serving static files            | `cargo run -p static-site`         |
| [`multi-flow`](./multi-flow)           | Multiple flows in one binary          | `cargo run -p multi-flow`          |
| [`middleware-chain`](./middleware-chain) | Composed middleware blocks         | `cargo run -p middleware-chain`    |
| [`wasmi-block`](./wasmi-block)         | Authoring a guest WASM block          | (see its README — built with `cargo build --target wasm32-wasip1`) |

Each example's README explains what to read in `src/` and which [wafer.run/docs](https://wafer.run/docs/quick-start) pages go deeper.
````

- [ ] **Step 3: Verify and commit**

```bash
wc -l examples/README.md                              # expect 12-20 lines
grep -c '\./hello-world' examples/README.md           # expect 2+
grep -c 'cargo run -p' examples/README.md             # expect 5
grep -c 'wafer-example-wasmi-echo' examples/README.md # expect 0 (we use the path label, not the binary name, in the table)
git add examples/README.md
git commit -m "docs: add examples index"
```

---

### Task 6: Add per-example READMEs

**Files:**
- Create: `examples/hello-world/README.md`
- Create: `examples/api-server/README.md`
- Create: `examples/middleware-chain/README.md`
- Create: `examples/multi-flow/README.md`
- Create: `examples/static-site/README.md`
- Create: `examples/wasmi-block/README.md`

This task adds 6 files in one commit. The first 5 share a uniform shape; `wasmi-block` differs (cdylib + standalone workspace + wasm32-wasip1 target).

- [ ] **Step 1: Read each example's `src/main.rs` (or `src/lib.rs`) to learn what it actually does**

```bash
for f in examples/*/src/main.rs examples/*/src/lib.rs; do
  echo "=== $f ==="
  head -40 "$f" 2>/dev/null
done
```

This is required reading — the per-example READMEs must accurately describe what the example demonstrates. If any example has drifted from a one-liner description (e.g., `multi-flow` is actually a single flow now), the README must reflect what the code does, not what its name implies.

- [ ] **Step 2: Write `examples/hello-world/README.md`**

```markdown
# hello-world

## What it demonstrates

The smallest end-to-end wafer setup: build a `Wafer`, register a single block, start it, and serve an HTTP endpoint that calls into the block. Use this as your first read after cloning.

## Run

```
cargo run -p hello-world
```

Then in another terminal:

```
curl http://localhost:8080/hello
```

(Adjust port if `src/main.rs` uses a different one — see the binding line near the top.)

## Key files

- `src/main.rs` — the entire example (block registration, wafer build, HTTP server boot).
- `Cargo.toml` — uses path deps into `../../crates/wafer-run` and `../../crates/wafer-flow-http-server`. In your own project you'd use crates.io versions.

## Related docs

- [wafer.run/docs/quick-start](https://wafer.run/docs/quick-start) — the recommended next read; same shape, more annotation.
- [wafer.run/docs/core-concepts](https://wafer.run/docs/core-concepts) — the mental model.
```

- [ ] **Step 3: Write `examples/api-server/README.md`**

```markdown
# api-server

## What it demonstrates

A small JSON REST API hosted by wafer. Shows how to expose flow endpoints behind HTTP routes and shape request/response as JSON.

## Run

```
cargo run -p api-server
```

Then:

```
curl http://localhost:8080/api/...        # see src/main.rs for the route table
```

## Key files

- `src/main.rs` — flow registration + HTTP server setup.
- `Cargo.toml` — same path-dep convention as `hello-world`.

## Related docs

- [wafer.run/docs/flow-configuration](https://wafer.run/docs/flow-configuration) — the flow config schema.
- [wafer.run/docs/built-in-blocks](https://wafer.run/docs/built-in-blocks) — first-party blocks you can compose.
```

- [ ] **Step 4: Write `examples/static-site/README.md`**

```markdown
# static-site

## What it demonstrates

Wafer serving static files via the first-party `web` block. Shows how to map URL paths to a directory on disk through a wafer flow.

## Run

```
cargo run -p static-site
```

Then visit `http://localhost:8080/` in a browser. The served files live alongside the example (check `src/main.rs` for the configured directory).

## Key files

- `src/main.rs` — `web` block configuration + flow wiring.
- `Cargo.toml` — path deps as in `hello-world`.

## Related docs

- [wafer.run/docs/built-in-blocks](https://wafer.run/docs/built-in-blocks) — `web` block reference.
- [wafer.run/docs/deployment](https://wafer.run/docs/deployment) — shipping a static-site wafer to a server.
```

- [ ] **Step 5: Write `examples/multi-flow/README.md`**

```markdown
# multi-flow

## What it demonstrates

Two or more flows running in a single wafer binary. Useful when one process needs to expose distinct request pipelines (e.g., a public API and an internal admin endpoint) without splitting binaries.

## Run

```
cargo run -p multi-flow
```

`src/main.rs` registers each flow and binds it to a route or port — read it to see which endpoints are live.

## Key files

- `src/main.rs` — all flows registered here; one block of code per flow.
- `Cargo.toml` — path deps.

## Related docs

- [wafer.run/docs/waferflow](https://wafer.run/docs/waferflow) — flow composition concepts.
- [wafer.run/docs/flow-configuration](https://wafer.run/docs/flow-configuration) — flow config schema.
```

- [ ] **Step 6: Write `examples/middleware-chain/README.md`**

```markdown
# middleware-chain

## What it demonstrates

Composing multiple middleware blocks (CORS, rate-limit, security-headers, etc.) ahead of a handler block. Shows the order rules and how earlier blocks short-circuit later ones.

## Run

```
cargo run -p middleware-chain
```

Then:

```
curl -i http://localhost:8080/...          # observe response headers from each middleware
```

## Key files

- `src/main.rs` — middleware registration order is the demo's whole point; read top-to-bottom.
- `Cargo.toml` — path deps.

## Related docs

- [wafer.run/docs/core-concepts](https://wafer.run/docs/core-concepts) — block ordering and short-circuit semantics.
- [wafer.run/docs/built-in-blocks](https://wafer.run/docs/built-in-blocks) — the middleware blocks used here.
```

- [ ] **Step 7: Write `examples/wasmi-block/README.md`**

This README differs from the others — `wasmi-block` is a `cdylib` compiled to WASM, in a standalone workspace, not a bin crate.

````markdown
# wasmi-block

## What it demonstrates

Authoring a guest WASM block: a Rust crate compiled to `wasm32-wasip1` that another wafer host loads at runtime via `wasmi`. The example block is an "echo" block that returns its input back to the caller.

This example is structured differently from the others:

- It's a `cdylib` (not a bin), compiled to a `.wasm` artifact.
- It's a **standalone Cargo workspace** (`[workspace]` block in its `Cargo.toml`) so it doesn't pollute the main workspace's target dir or feature unification.
- It builds via `cargo build --target wasm32-wasip1`, not `cargo run`.

## Build

From inside the example's directory:

```
cd examples/wasmi-block
rustup target add wasm32-wasip1                            # one-time
cargo build --release --target wasm32-wasip1
```

The artifact lands at:

```
examples/wasmi-block/target/wasm32-wasip1/release/wafer_example_wasmi_echo.wasm
```

## Use the artifact

The produced `.wasm` is the input to wafer's WASM block loader. It's also what `crates/wafer-run/testdata/echo_block.wasm` is regenerated from when running `wafer-run`'s integration tests — see [`CONTRIBUTING.md`](../../CONTRIBUTING.md) → "Build & test" → testdata gotcha for the copy command.

## Key files

- `src/lib.rs` — the block implementation (uses `#[wafer_block]` from `wafer-sdk`).
- `Cargo.toml` — note `[lib] crate-type = ["cdylib"]` and the `[workspace]` block.
- `manifest.json` — block manifest used by the registry.
- `tests/` — host-side tests that load the built `.wasm`.

## Related docs

- [wafer.run/docs/creating-a-block](https://wafer.run/docs/creating-a-block) — block author's guide.
- [wafer.run/docs/wasm-blocks](https://wafer.run/docs/wasm-blocks) — runtime details for WASM blocks.
````

- [ ] **Step 8: Verify and commit**

```bash
for d in examples/*/; do
  test -f "$d/README.md" && echo "ok $d" || echo "MISSING $d"
done

# All 6 example READMEs should match the uniform shape (heading + 4 sections):
for f in examples/*/README.md; do
  echo "=== $f ==="
  grep -c '^## ' "$f"      # expect 4 for hello-world/api-server/static-site/multi-flow/middleware-chain
                            # expect 5 for wasmi-block (extra "Use the artifact" section)
done

# Cross-reference checks:
grep -l 'cargo run -p' examples/hello-world/README.md examples/api-server/README.md examples/static-site/README.md examples/multi-flow/README.md examples/middleware-chain/README.md
grep -l 'cargo build --release --target wasm32-wasip1' examples/wasmi-block/README.md

git add examples/hello-world/README.md examples/api-server/README.md examples/middleware-chain/README.md examples/multi-flow/README.md examples/static-site/README.md examples/wasmi-block/README.md
git status                                                # confirm only those 6 files staged
git commit -m "docs: add per-example READMEs"
```

Expected:
- All 6 `ok ...` lines.
- 4 sections each for the 5 binary examples; 5 sections for `wasmi-block`.
- All 5 binary-example READMEs contain `cargo run -p`; `wasmi-block` README contains the wasm32-wasip1 build command.

---

### Task 7: Verification — link consistency, file paths, build sanity

**Files:** none (verification only — no commits unless a real error is found and fixed).

- [ ] **Step 1: Every `wafer.run/docs/<page>` link points at a real page**

Site docs live at `/home/joris/Programs/suppers-ai/workspace/site/content/docs/`. Verify every doc link resolves:

```bash
SITE_DOCS=/home/joris/Programs/suppers-ai/workspace/site/content/docs
ls "$SITE_DOCS" | sed 's/\.html$//' | sort > /tmp/site-docs.txt
grep -ohE 'wafer\.run/docs/[a-z-]+' README.md CONTRIBUTING.md examples/README.md examples/*/README.md \
  | sed 's|wafer.run/docs/||' | sort -u > /tmp/cited-docs.txt
echo "=== Cited docs not present on the site ==="
comm -23 /tmp/cited-docs.txt /tmp/site-docs.txt
```

Expected: empty output. Any line printed is a broken link — fix the citing file (either change the link or pick a different page that exists).

If `wafer.run/docs/quick-start` is cited but the site has it at `wafer.run/docs/quick-start.html` only — that's fine; the site serves the page without `.html`. The check above strips `.html` from both sides.

- [ ] **Step 2: Every cited repo path exists**

```bash
for path in CONTRIBUTING.md examples/README.md crates/wafer-run/README.md \
            crates/wafer-block crates/wafer-block-macro crates/wafer-cli \
            crates/wafer-flow crates/wafer-flow-http-server crates/wafer-core \
            crates/wafer-test-support crates/wafer-sql-utils crates/wafer-ffi \
            crates/wafer-run-node sdks/rust packages/wafer-client-js \
            registry common go docs/specs docs/plans \
            examples/hello-world examples/api-server examples/middleware-chain \
            examples/multi-flow examples/static-site examples/wasmi-block \
            LICENSE README.md; do
  test -e "$path" && echo "ok $path" || echo "MISSING $path"
done | grep -E '^MISSING' && echo "FAIL: at least one cited path is missing" || echo "all paths exist"
```

Expected: `all paths exist`.

- [ ] **Step 3: Build sanity check**

```bash
cargo build --workspace 2>&1 | tail -5
```

Expected: `Finished ...` (success). Doc-only changes shouldn't break compilation; if this fails, something else is wrong.

- [ ] **Step 4: CI-mirror test command (sanity, no regression)**

```bash
cargo test --workspace --exclude wafer-run && cargo test -p wafer-run --lib 2>&1 | tail -15
```

Expected: all tests pass (or, if some tests were already failing on `main`, the same set fails — no new failures from this branch). If you see new failures, this is a CI environment issue or merge skew, not a docs issue.

- [ ] **Step 5: nightly fmt is a no-op**

```bash
cargo +nightly fmt --all -- --check
echo "exit=$?"
```

Expected: `exit=0`. Markdown isn't formatted by rustfmt, so this should pass without touching anything.

- [ ] **Step 6: If any check fails**

- Broken doc link: pick the closest-matching page on the site, or remove the link if no equivalent exists.
- Missing cited path: either the path is wrong (fix the README/CONTRIBUTING) or the path was deleted upstream (decide whether to drop the citation or open a follow-up).
- Build break: this is unexpected for a doc-only branch. Check `git diff main..` for accidental edits to non-doc files. If a real Rust file was touched by accident, `git restore` it and rebuild.
- Test failure: re-run on `main` — if it fails there too, it's a pre-existing issue, document in the PR description and proceed.

If a fix is small and obvious, fix in place and amend the relevant earlier commit (or add a separate `fix(docs):` commit if amending crosses a Task boundary). Otherwise escalate as BLOCKED.

---

### Task 8: Push branch + open PR (gated)

**Files:** none

This task pushes shared state to GitHub. Pause for explicit user confirmation before any remote action.

- [ ] **Step 1: Show user the local state**

```bash
git log --oneline main..feat/docs-3a
git diff --stat main..feat/docs-3a
```

Expected: 6-7 commits, 11 files added (LICENSE, README.md, CONTRIBUTING.md, examples/README.md, 6× per-example README, plus the spec + plan if they were committed in Task 1 Step 2).

Wait for user "go ahead".

- [ ] **Step 2: Push the branch**

```bash
git push -u origin feat/docs-3a 2>&1 | tail -5
```

Expected: `branch 'feat/docs-3a' set up to track 'origin/feat/docs-3a'.`

- [ ] **Step 3: Open the PR**

```bash
gh pr create --title "docs: add root README, CONTRIBUTING, LICENSE, and per-example READMEs (Spec 3A)" --body "$(cat <<'EOF'
## Summary

Closes the repo-level documentation gap identified in [Spec 3A](./docs/specs/2026-04-30-wafer-run-docs-3a-design.md).

A reader landing on `github.com/wafer-run/wafer-run` cold today sees a directory listing with no README. This PR adds:

- **`README.md`** — billboard for first-time GitHub visitors. WAFER tagline, 5-bullet feature highlights, illustrative code snippet, links to wafer.run/docs and `examples/hello-world`.
- **`CONTRIBUTING.md`** — dev onboarding. Toolchain (stable + nightly + `wasm32-wasip1`), build & test (CI-mirror command), the `echo_block.wasm` testdata gotcha, repo layout, code style, branch + PR workflow, worktrees.
- **`LICENSE`** — verbatim MIT, backs the existing `Cargo.toml` `license = "MIT"` declaration. (Discovered during fact-gathering: the license claim had no file to back it.)
- **`examples/README.md`** — table-of-examples index.
- **`examples/<name>/README.md`** — uniform 4-section README in each of the 6 examples (`wasmi-block` gets a 5th section because it's a cdylib in a standalone workspace).

User-facing tutorial content stays on [wafer.run/docs](https://wafer.run/docs/quick-start) — repo docs link out rather than duplicate.

## Spec & plan

- Design: `docs/specs/2026-04-30-wafer-run-docs-3a-design.md`
- Plan: `docs/plans/2026-04-30-wafer-run-docs-3a.md`

## Test plan

- [ ] `cargo build --workspace` passes (sanity).
- [ ] `cargo test --workspace --exclude wafer-run && cargo test -p wafer-run --lib` passes.
- [ ] `cargo +nightly fmt --all -- --check` passes (no-op for markdown).
- [ ] All `wafer.run/docs/<page>` references in new files point to a real page in `wafer-run/site` `content/docs/`.
- [ ] All cited repo paths exist on `feat/docs-3a`.

## Initiative context

This is sub-spec A of Spec 3 (Developer Experience). Spec 3B (`wafer dev` + hot-reload) is queued behind this. See `docs/specs/2026-04-30-wafer-run-docs-3a-design.md` and the workspace meta-repo memory `wafer-run-hardening-state.md` for the full initiative outline.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 4: Verify**

```bash
gh pr view --json url --jq .url
```

---

## Self-review

**Spec coverage:**

| Spec section / requirement                                 | Implemented in   |
| ---------------------------------------------------------- | ---------------- |
| `LICENSE` (MIT)                                            | Task 2           |
| `README.md` — title + tagline                              | Task 3 Step 2    |
| `README.md` — 5 feature bullets                            | Task 3 Step 2    |
| `README.md` — quick-taste snippet                          | Task 3 Step 2-3  |
| `README.md` — get-started 3-link table                     | Task 3 Step 2    |
| `README.md` — repo layout                                  | Task 3 Step 2    |
| `README.md` — license + status                             | Task 3 Step 2    |
| `CONTRIBUTING.md` — toolchain (stable + nightly + wasm)    | Task 4 Step 2    |
| `CONTRIBUTING.md` — build & test (CI-mirror command)       | Task 4 Step 2    |
| `CONTRIBUTING.md` — testdata gotcha                        | Task 4 Step 2    |
| `CONTRIBUTING.md` — repo layout                            | Task 4 Step 2    |
| `CONTRIBUTING.md` — code style (poll_once, raw SQL, etc.)  | Task 4 Step 2    |
| `CONTRIBUTING.md` — branch + PR workflow + footgun warning | Task 4 Step 2    |
| `CONTRIBUTING.md` — worktrees                              | Task 4 Step 2    |
| `examples/README.md` — table index                         | Task 5 Step 2    |
| 5× uniform per-example README                              | Task 6 Steps 2-6 |
| `wasmi-block` README (different shape: cdylib, wasm32)     | Task 6 Step 7    |
| Verification — every doc link resolves                     | Task 7 Step 1    |
| Verification — every cited path exists                     | Task 7 Step 2    |
| Verification — build still passes                          | Task 7 Step 3    |
| Branch + PR (workspace rule)                               | Tasks 1, 8       |

**Placeholder scan:** No `TBD` / `TODO` / "implement later". Each task has full file content or exact commands. The Task 6 examples are described concretely; Task 6 Step 1 explicitly tells the executor to read each `src/main.rs` first to verify the description matches reality before writing.

**Type / identifier consistency:**
- Branch name `feat/docs-3a` consistent across Task 1 and Task 8.
- File paths consistent: `LICENSE`, `README.md`, `CONTRIBUTING.md`, `examples/README.md`, `examples/<name>/README.md`.
- Test command `cargo test --workspace --exclude wafer-run && cargo test -p wafer-run --lib` consistent in CONTRIBUTING.md (Task 4) and verification (Task 7).
- `wasmi-block` consistently labeled cdylib + standalone workspace + `wasm32-wasip1` target across spec, examples/README.md table, and `examples/wasmi-block/README.md`.
- Footgun warning text identical in Task 1 and CONTRIBUTING.md.

**Out-of-scope creep:** None. No site changes (Q4 = A). No per-crate READMEs added beyond what already exists. No CI/lint changes. No Rust source touched.
