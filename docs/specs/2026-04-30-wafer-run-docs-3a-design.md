# wafer-run Spec 3A — Repo Documentation

**Status:** Draft (2026-04-30)
**Initiative:** Hardening Spec 3 (Developer Experience), sub-spec A (Documentation).
**Predecessors:** Spec 1 (PR #3), Spec 2A (PR #4), Spec 2B (PR #5) — merged.
**Successor:** Spec 3B — `wafer dev` + hot-reload (queued behind this spec).

## Goal

Close the repo-level documentation gap. A reader who lands on `github.com/wafer-run/wafer-run` cold today sees a directory listing with no README. This spec adds the files needed for two audiences:

- **Audience A — first-time evaluator on GitHub.** "What is this, why care, where do I go next?" Funnels to `wafer.run/docs` and to a runnable example.
- **Audience B — new contributor.** "How do I build, test, and ship a change here?" Onboarding for someone who has cloned the repo to hack on wafer-run itself.

User-facing tutorial content is explicitly NOT in scope — `wafer.run/docs` already covers quick-start, core concepts, creating-a-block, CLI, registry, deployment, waferflow, block-capabilities, and ~14 more pages. The repo docs link to those pages rather than duplicate them.

## Non-goals

- No new content on `wafer.run` (the site). If a site gap surfaces while writing, defer it to a follow-up PR.
- No per-crate READMEs beyond what already exists (`crates/wafer-run/README.md`, `common/README.md`, `packages/wafer-client-js/README.md` stay as-is).
- No architecture diagrams. The repo-layout tree in README + CONTRIBUTING is sufficient orientation for both audiences.
- No changelog or migration guide (pre-1.0).
- No CI / lint / test changes. This is a documentation-only PR.

## Deliverables

**10 files added** (no edits to existing files except `.gitignore` if required):

```
LICENSE                                (MIT, backs the existing Cargo.toml `license = "MIT"`)
README.md                              (billboard, audience A)
CONTRIBUTING.md                        (dev onboarding, audience B)
examples/README.md                     (1-line index of the 6 examples)
examples/hello-world/README.md
examples/api-server/README.md
examples/middleware-chain/README.md
examples/multi-flow/README.md
examples/static-site/README.md
examples/wasmi-block/README.md
```

The `LICENSE` addition was scoped in during fact-gathering: `Cargo.toml` declares `license = "MIT"` but no LICENSE file exists. Shipping a README that links to a non-existent LICENSE would propagate the gap.

## Reader flow

```
GitHub visitor lands on README.md
    │
    ├── "What is this?"            → top of README (pitch + features)
    ├── "Show me code"             → quick-taste snippet in README
    ├── "Run something"            → examples/hello-world/README.md
    ├── "Read deeper"              → wafer.run/docs/quick-start
    └── "I want to contribute"     → CONTRIBUTING.md

Contributor reads CONTRIBUTING.md
    │
    ├── "What's my toolchain?"     → toolchain section
    ├── "Build & test commands"    → build/test section
    ├── "Where does X live?"       → repo-layout section
    ├── "How do I run an example?" → examples/hello-world/README.md
    ├── "What's the PR process?"   → branch+PR section
    └── "Mental model of wafer"    → wafer.run/docs/core-concepts (no duplication)

Direct landing on examples/<name>/
    │
    ├── "What does this demo?"     → "What it demonstrates" section
    ├── "How do I run it?"         → "Run" section
    ├── "What should I read?"      → "Key files" section
    └── "Deeper docs?"             → wafer.run/docs/<relevant page>
```

## Per-file content specification

### `LICENSE`

Verbatim MIT license text with copyright line `Copyright (c) 2026 wafer.run contributors`. No edits to `Cargo.toml` (it already says `license = "MIT"`).

### `README.md` (~80-120 lines)

Section order:

1. **Title + tagline.** `# wafer.run` and the WAFER acronym ("WebAssembly Architecture for Flow Execution & Routing"), one-paragraph elevator pitch matching the wafer.run hero.
2. **Why wafer.** ~5 bullets of feature highlights drawn from Cargo.toml workspace (single binary; WASM blocks via wasmi; flow composition; package registry with `wafer-cli`; secure-by-default with WRAP + block capabilities). No marketing prose — each bullet says what the feature is and links to the wafer.run/docs page that explains it.
3. **Quick taste.** A 5-15 line bespoke snippet illustrating the shape of wafer usage (build a `Wafer`, register a block, call into it). Hand-written for the README's billboard tone — explicitly NOT copied from `examples/hello-world/src/main.rs` (so they don't drift). The snippet's purpose is illustrative, not runnable; the next paragraph directs readers to `examples/hello-world` for the actual runnable version.
4. **Get started.** Three links, in order:
   - `→ wafer.run/docs/quick-start` for the user docs
   - `→ examples/hello-world` for "clone and run"
   - `→ CONTRIBUTING.md` for "I want to hack on wafer-run"
5. **Repo layout.** A short tree showing top-level directories (`crates/`, `examples/`, `sdks/`, `packages/`, `registry/`, `docs/`, `common/`, `go/`) with one-line descriptions. Not exhaustive — this is orientation, not a manifest.
6. **License.** Single line: "MIT — see `LICENSE`."
7. **Status.** One paragraph: pre-1.0, breaking changes possible, registry currently private.

Constraints:
- No section may exceed ~25 lines. README is a billboard.
- All external links use `https://wafer.run/docs/...` form (not relative paths to `content/docs/*.html` in a sibling repo).
- No badges in v1 (no CI, no crates.io, no docs.rs yet — adding badges that 404 hurts more than helps).

### `CONTRIBUTING.md` (~200-300 lines)

Section order:

1. **Welcome.** One paragraph: PR-driven, pre-1.0, link to issues for bug reports and to discussions/issues for design questions.
2. **Toolchain.**
   - Rust stable (no `rust-toolchain.toml` — caller's responsibility).
   - Nightly Rust required for `cargo +nightly fmt --all` (CI Format & Lint enforces nightly-only rustfmt rules from `rustfmt.toml`: `imports_granularity = Crate`, `group_imports = StdExternalCrate`).
   - `wasm32-wasip1` target for WASM block development: `rustup target add wasm32-wasip1`.
   - Node 20+ for `packages/wafer-client-js`.
3. **Build & test.**
   - `cargo build --workspace` — builds everything except `examples/wasmi-block` (standalone workspace).
   - **Default test command (mirrors CI):**
     ```
     cargo test --workspace --exclude wafer-run && cargo test -p wafer-run --lib
     ```
     Don't use `cargo test --workspace` directly — it will compile `wafer-run`'s integration tests, including `wasmi_block_test.rs`, which depends on a WASM testdata fixture (see gotcha below).
   - **Gotcha — `crates/wafer-run/testdata/echo_block.wasm`.** This file is `.gitignore`d (`*.wasm` rule) and is NOT tracked. A fresh clone will lack it. `wasmi_block_test.rs` `include_bytes!`s it, so `cargo test -p wafer-run --tests` and `cargo clippy --all-targets` will fail with a missing-file error. Two fixes:
     - **CI-mirror approach (recommended for everyday work):** use the test command above, which only runs `wafer-run`'s `--lib` tests.
     - **Generate the fixture (needed if you're modifying `wafer-run`'s integration tests):**
       ```
       (cd examples/wasmi-block && cargo build --release --target wasm32-wasip1)
       cp examples/wasmi-block/target/wasm32-wasip1/release/wafer_example_wasmi_echo.wasm \
          crates/wafer-run/testdata/echo_block.wasm
       ```
     The pre-commit hook runs `cargo clippy --all-targets --fix`; if you don't have the fixture, the hook will fail. Generate the fixture once after cloning and the file persists in your worktree.
4. **Repo layout.** By directory, what lives where, links to per-crate READMEs as they exist:
   - `crates/wafer-run/` — runtime entry point. See its README.
   - `crates/wafer-block*/` — first-party blocks. Each is a separate crate.
   - `crates/wafer-cli/` — CLI binary (`wafer search`, `wafer info`, `wafer install`, `wafer publish`).
   - `crates/wafer-flow*/` — flow composition + HTTP server.
   - `crates/wafer-core/` — runtime core (also lives elsewhere; check current state).
   - `crates/wafer-test-support/` — test fakes (`FakeDb`, `FakeCrypto`, `WaferBuilder`).
   - `sdks/rust/` — `wafer-sdk` for guest WASM blocks (`#[wafer_block]` proc macro consumer).
   - `packages/wafer-client-js/` — TS/Node client. See its README.
   - `examples/` — runnable demos. See `examples/README.md`.
   - `registry/` — package registry data.
   - `common/` — shared resources. See its README.
   - `docs/` — specs and plans. `docs/specs/` for design docs, `docs/plans/` for implementation plans.
5. **Code style.**
   - `cargo fmt` (stable for local; `cargo +nightly fmt --all` before push or CI fails).
   - `cargo clippy --all-targets --all-features` clean.
   - **No `poll_once` / `block_on` sync bridges** — async callers must remain async (cite `CLAUDE.md` rule).
   - **No raw SQL** in block code — use `wafer-sql-utils` builders. Exceptions: SQL explorer admin, migration runners, test fixtures.
   - **No hardcoded domain values** — use `ConfigVar`.
   - **Fix at root cause** — no compat shims, no quick fixes for code smells.
6. **Branch + PR workflow.**
   - Branch from `main`: `git checkout -b feat/<topic>` (NOT `git checkout -b feat/<topic> origin/main` — that sets upstream to origin/main and `git push` then pushes to main directly).
   - Conventional commit prefix in messages: `feat:`, `fix:`, `chore:`, `docs:`, `refactor:`, `test:`, `ci:`.
   - PR title mirrors the commit prefix.
   - Pre-commit hook runs `cargo fmt` + `cargo clippy --all-targets --fix`. Don't bypass with `--no-verify`.
7. **Worktrees for parallel work.** Workspace convention: each spec gets its own worktree at `/workspace/wafer-run-<topic>/` on branch `feat/<topic>`. Useful when running multiple in-flight branches without context-switching the main checkout.
8. **Where to ask.** Issues at `github.com/wafer-run/wafer-run/issues`. Specs and plans live under `docs/specs/` and `docs/plans/`.

Constraints:
- All commands shown must be exact as a copy-paste reader would type them (no `<placeholder>` style unless surrounded by explicit "replace with"-type instructions).
- Don't duplicate what `wafer.run/docs/core-concepts` covers — link out instead.

### `examples/README.md` (~25 lines)

A short index. Format:

```
# wafer-run examples

Each subdirectory is a self-contained, runnable example. New to wafer? Start with **hello-world**.

| Example | What it demonstrates | Run |
|---|---|---|
| [`hello-world`](./hello-world)         | Smallest possible wafer + HTTP server | `cargo run -p hello-world` |
| [`api-server`](./api-server)           | Wafer behind a JSON REST API          | `cargo run -p api-server`  |
| [`static-site`](./static-site)         | Wafer serving static files            | `cargo run -p static-site` |
| [`multi-flow`](./multi-flow)           | Multiple flows in one binary          | `cargo run -p multi-flow`  |
| [`middleware-chain`](./middleware-chain) | Composed middleware blocks          | `cargo run -p middleware-chain` |
| [`wasmi-block`](./wasmi-block)         | Authoring a guest WASM block          | (see its README — built with `cargo build --target wasm32-wasip1`) |

Each example's README explains what to read in `src/` and which `wafer.run/docs` pages go deeper.
```

### `examples/<name>/README.md` (~25-40 lines each, uniform skeleton)

```
# <name>

## What it demonstrates

<1 paragraph: the specific concept this example shows. Concrete, not "various wafer features".>

## Run

<exact command(s); for hello-world: `cargo run -p hello-world` then `curl http://localhost:8080/...`>

## Key files

- `src/main.rs` (or `src/lib.rs` for wasmi-block) — <one-line description of what to look at>
- `Cargo.toml` — <only mention if there's something noteworthy, e.g., wasmi-block being a standalone workspace>

## Related docs

- [wafer.run/docs/<page>](https://wafer.run/docs/<page>) — <why this page is relevant>
- (1-2 more links as appropriate)
```

Per-example content notes:

- **hello-world** — smallest end-to-end. Run command + curl example. Links to `quick-start`.
- **api-server** — JSON REST. Links to `flow-configuration` + `built-in-blocks`.
- **static-site** — serving files via the `web` block. Links to `built-in-blocks` (web block) + `deployment`.
- **multi-flow** — multiple flows in one binary. Links to `waferflow` + `flow-configuration`.
- **middleware-chain** — composed middleware. Links to `core-concepts` + `built-in-blocks`.
- **wasmi-block** — authoring a guest WASM block. Different from the others: it's a `cdylib` in a standalone workspace at `examples/wasmi-block/`. Build command is `cargo build --target wasm32-wasip1` (NOT `cargo run`). Result is a `.wasm` artifact that another wafer instance loads. Links to `creating-a-block` + `wasm-blocks`.

## Verification

Documentation correctness checks (the implementation plan should run these):

1. **Every link resolves.** All `wafer.run/docs/<page>` references must point to a real `content/docs/<page>.html` in the `wafer-run/site` repo. The plan should grep across the new files and check each path against the site repo's `content/docs/` directory listing (already enumerated above: 22 pages).
2. **Every cited file path exists.** Every path referenced in CONTRIBUTING.md (e.g., `crates/wafer-test-support/`, `sdks/rust/`, `packages/wafer-client-js/`) is checked against `git ls-files`.
3. **Every example's run command works on a fresh clone.** Build (`cargo run -p <name> --release`) is enough — no need to exercise the running example. `examples/wasmi-block` is built with `cargo build --target wasm32-wasip1` from inside its directory.
4. **Quick-taste code in README.md.** Bespoke illustrative snippet (not copied from any example file). The plan should review it once for shape correctness against current `wafer-run` API, but no sync requirement is established — README's snippet is documentation, not a runnable test.
5. **CI Format & Lint.** Markdown files are fine, but the plan must `cargo +nightly fmt --all` once anyway (a no-op for non-Rust changes — confirms the gotcha rule).
6. **`cargo build --workspace`** still passes (sanity check; should be a no-op since no Rust changed).

## Open questions

None. All scope decisions are made:
- Audience: A + B (Q1).
- Files: README + CONTRIBUTING (no separate GETTING_STARTED) (Q2).
- Per-example READMEs: all 6 + index (Q3).
- Site additions: descoped (Q4).
- LICENSE: in-scope (fact-gathering finding).

## Implementation cadence

Single PR. The 10 files are independent and small enough to land together; splitting wouldn't reduce review burden and would create a window where the README points at unwritten files.

Branch: `feat/docs-3a` from `main`.
PR title: `docs: add root README, CONTRIBUTING, LICENSE, and per-example READMEs`.
