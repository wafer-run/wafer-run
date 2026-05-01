# JS SDK cleanup — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the half-implemented JavaScript/TypeScript block subsystem from `wafer-run` so the codebase no longer ships a partial runtime that throws on host-import calls.

**Architecture:** Pure deletion across four crates and one npm workspace member. No new code, no new tests, no new behaviour. The only behaviour change visible to users is `wafer new --lang ts` rejecting with `"Unknown language. Supported: rust, go"` instead of generating a half-working scaffold.

**Tech Stack:** Rust (cargo, clippy, nightly rustfmt), Node (npm workspaces). No new dependencies; existing dependencies unchanged.

**Spec:** `docs/specs/2026-05-01-js-sdk-cleanup-design.md`.

**Worktree:** `/workspace/wafer-run-js-sdk-cleanup/` on branch `feat/js-sdk-cleanup`. The gitignored `crates/wafer-run/testdata/echo_block.wasm` has already been copied in to satisfy the pre-commit hook.

---

## File map (everything that changes)

| File | Action |
|---|---|
| `sdks/js/` (whole directory: `src/index.ts`, `src/host.ts`, `src/types.ts`, `package.json`, `tsconfig.json`, `README.md` if any) | DELETE |
| `package.json` (root) | EDIT — remove `"sdks/js"` from `workspaces` |
| `package-lock.json` (root) | REGENERATE via `npm install` |
| `crates/wafer-cli/src/validate.rs` | EDIT — remove `poll_oneoff` and `sched_yield` WASI stubs (lines 181-209) |
| `crates/wafer-cli/src/build.rs` | EDIT — remove `Lang::TypeScript` match arm (line 52) and `fn build_typescript` (line 167-…). Keep `fn find_wasm_in_dir` (still used by Rust path at line 130) |
| `crates/wafer-cli/src/scaffold.rs` | EDIT — remove `Lang::TypeScript` arm in `fn scaffold` (line 38), the print block at lines 53-60, the `// TypeScript scaffold` header (line 182), `fn scaffold_typescript` (line 185-296), and `const RUNTIME_LIB_RS` (line 302-469). Note: the `with_context` / `pack_ptr_len` / `string_to_packed` items the spec flagged are *inside* the `RUNTIME_LIB_RS` raw string — they vanish with the constant, no separate work. |
| `crates/wafer-cli/src/detect.rs` | EDIT — remove `Lang::TypeScript` variant (line 10), `"typescript" \| "ts"` parse arm (line 19), update error message at line 20, doc-comment line 32, and `package.json` detection branch (lines 40-42) |
| `crates/wafer-cli/src/main.rs` | EDIT — update doc-comment at line 40 from `"Programming language: rust | go | typescript."` to `"Programming language: rust | go."` |

Estimated diff: ~600 LOC removed, 0 added (excluding `package-lock.json` regeneration).

---

## Conventions for every task

- **Branch:** `feat/js-sdk-cleanup` (already created).
- **Commit style:** match recent history — short imperative subject, optional body. Sign with the workspace co-author trailer.
- **Pre-commit:** the hook runs `cargo fmt` + `cargo clippy --all-targets --fix` automatically. If it modifies files, re-stage and re-commit.
- **Before pushing the branch:** run `cargo +nightly fmt --all` once and commit the result if anything changes (CI Format & Lint uses nightly rustfmt with `imports_granularity = Crate` + `group_imports = StdExternalCrate` from `rustfmt.toml`; stable rustfmt silently ignores those).

---

### Task 1: Baseline verification

**Files:** none (read-only).

**Goal:** Capture the pre-deletion state so we can verify the cleanup is complete and didn't break anything.

- [ ] **Step 1: Confirm worktree + branch.**

  Run: `pwd && git branch --show-current`
  Expected:
  ```
  /home/joris/Programs/suppers-ai/workspace/wafer-run-js-sdk-cleanup
  feat/js-sdk-cleanup
  ```

- [ ] **Step 2: Capture baseline grep count for sweep targets.**

  Run: `git grep -i 'typescript\|sdks/js\|@wafer-run/sdk\|boa_engine' | wc -l`
  Expected: a non-zero count (likely 20-40). Record it. After Task 8 it must be 0 (excluding the spec/plan docs themselves).

- [ ] **Step 3: Confirm baseline tests pass.**

  Run: `cargo test --workspace --exclude wafer-run && cargo test -p wafer-run --lib`
  Expected: PASS. (CI-mirror command from workspace conventions.)

- [ ] **Step 4: Confirm baseline clippy passes.**

  Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20`
  Expected: clean exit. If it fails on pre-existing warnings unrelated to this work, record them — they are not for this PR to fix.

- [ ] **Step 5: No commit — this task is verification only.**

---

### Task 2: Delete `sdks/js` and update npm workspace

**Files:**
- Delete: `sdks/js/` (entire directory)
- Modify: `package.json`
- Regenerate: `package-lock.json`

**Goal:** Remove the unpublished `@wafer-run/sdk` npm package and its workspace registration. No Rust code references it (verified during brainstorming — only `crates/wafer-cli/src/scaffold.rs` does, which Task 5 will clean up).

- [ ] **Step 1: Verify `sdks/js` has no consumer outside `scaffold.rs`.**

  Run: `git grep -ln '@wafer-run/sdk\|sdks/js' -- ':!sdks/js' ':!docs' ':!package.json' ':!package-lock.json'`
  Expected: only `crates/wafer-cli/src/scaffold.rs` (where the scaffolded TS template references `@wafer-run/sdk`).

  If any other file is listed (e.g., a real consumer in `examples/`), STOP and surface it before deleting.

- [ ] **Step 2: Delete the directory.**

  Run: `git rm -r sdks/js`
  Expected: deletion of all tracked files under `sdks/js/`.

- [ ] **Step 3: Edit `package.json` to drop `"sdks/js"` from `workspaces`.**

  The current file:
  ```json
  {
    "name": "wafer-run-monorepo",
    "private": true,
    "workspaces": [
      "packages/wafer-client-js",
      "sdks/js",
      "crates/wafer-run-node",
      "crates/wafer-site"
    ]
  }
  ```
  Becomes:
  ```json
  {
    "name": "wafer-run-monorepo",
    "private": true,
    "workspaces": [
      "packages/wafer-client-js",
      "crates/wafer-run-node",
      "crates/wafer-site"
    ]
  }
  ```

- [ ] **Step 4: Regenerate `package-lock.json`.**

  Run: `npm install`
  Expected: completes without error. `package-lock.json` will have many lines change — that is normal lockfile churn.

- [ ] **Step 5: Confirm `sdks/js` is gone from `git ls-files`.**

  Run: `git ls-files sdks/ | head`
  Expected: empty output.

- [ ] **Step 6: Stage and commit.**

  ```bash
  git add -A sdks/ package.json package-lock.json
  git commit -m "$(cat <<'EOF'
  refactor: remove unpublished @wafer-run/sdk npm package (Spec 3C)

  The package has no published consumers (root package.json is private; the
  package itself has no publishConfig). Its host imports throw at runtime,
  it has no examples, and no docs reference it. See
  docs/specs/2026-05-01-js-sdk-cleanup-design.md for context.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

  If the pre-commit hook modifies anything, re-stage and re-commit (NEW commit, not `--amend`).

---

### Task 3: Remove boa-only WASI stubs from `validate.rs`

**Files:**
- Modify: `crates/wafer-cli/src/validate.rs:181-209`

**Goal:** The `poll_oneoff` and `sched_yield` WASI stubs exist solely to satisfy boa-engine-compiled WASM modules (per their inline comments). With the JS pipeline going away, they become dead host bindings.

- [ ] **Step 1: Confirm the stubs' only justification is the JS pipeline.**

  Run: `grep -B1 -A2 'poll_oneoff\|sched_yield' crates/wafer-cli/src/validate.rs`
  Expected: shows the comments `"Required by boa_engine-compiled WASM modules (JS/TS blocks)."` for both stubs.

- [ ] **Step 2: Delete lines 181 through 209 inclusive.**

  Remove this exact block (between the `random_get` stub at line 179's closing `?;` and the `// 3. Instantiate.` comment at line 211):

  ```rust
      // wasi_snapshot_preview1::poll_oneoff(in_ptr, out_ptr, nsubscriptions, nevents_ptr) -> errno
      // Required by boa_engine-compiled WASM modules (JS/TS blocks).
      linker
          .func_wrap(
              "wasi_snapshot_preview1",
              "poll_oneoff",
              |mut caller: Caller<()>,
               _in_ptr: i32,
               _out_ptr: i32,
               _nsubscriptions: i32,
               nevents_ptr: i32|
               -> i32 {
                  if let Some(wasmi::Extern::Memory(memory)) = caller.get_export("memory") {
                      let _ = memory.write(&mut caller, nevents_ptr as usize, &0u32.to_le_bytes());
                  }
                  0
              },
          )
          .context("Failed to define poll_oneoff stub")?;

      // wasi_snapshot_preview1::sched_yield() -> errno
      // Required by boa_engine-compiled WASM modules (JS/TS blocks).
      linker
          .func_wrap(
              "wasi_snapshot_preview1",
              "sched_yield",
              |_: Caller<()>| -> i32 { 0 },
          )
          .context("Failed to define sched_yield stub")?;

  ```

- [ ] **Step 3: Verify `validate.rs` still compiles in isolation.**

  Run: `cargo check -p wafer-cli`
  Expected: PASS.

- [ ] **Step 4: Run `wafer-cli` tests.**

  Run: `cargo test -p wafer-cli`
  Expected: PASS.

- [ ] **Step 5: Stage and commit.**

  ```bash
  git add crates/wafer-cli/src/validate.rs
  git commit -m "$(cat <<'EOF'
  refactor(cli): remove boa-only WASI stubs from validate (Spec 3C)

  poll_oneoff and sched_yield stubs were registered only for JS blocks
  compiled via boa_engine. With that pipeline removed, the bindings are
  dead. Rust- and Go-compiled WASM blocks do not call them.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 4: Strip TypeScript path from `build.rs`

**Files:**
- Modify: `crates/wafer-cli/src/build.rs`

**Goal:** Remove the `Lang::TypeScript` dispatch arm and the entire `build_typescript` function. **Keep** `find_wasm_in_dir` — it is still called by the Rust build path (line 130).

- [ ] **Step 1: Re-verify `find_wasm_in_dir` retains a Rust caller.**

  Run: `grep -n 'find_wasm_in_dir' crates/wafer-cli/src/build.rs`
  Expected: at least one call site that is NOT inside `build_typescript`. (Currently lines 130 and 247; line 130 is the Rust path.)

  If only the TS path calls it, remove `find_wasm_in_dir` too. If a Rust caller remains, keep the function.

- [ ] **Step 2: Locate and remove the `Lang::TypeScript` match arm.**

  At line 52, the dispatch reads:
  ```rust
          Lang::TypeScript => build_typescript(dir, &block_wasm_path)?,
  ```
  Delete that line.

- [ ] **Step 3: Locate and remove `fn build_typescript`.**

  Starting at line 167 (`fn build_typescript(dir: &Path, out: &Path) -> anyhow::Result<()>`), delete the entire function body up to and including its closing `}` (around line 264, immediately before `fn find_wasm_in_dir`). Do not delete `fn find_wasm_in_dir`.

  After deletion, the next item in the file should be the unmodified `fn find_wasm_in_dir(...)`.

- [ ] **Step 4: Remove any now-unused imports.**

  Run: `cargo check -p wafer-cli 2>&1 | tail -30`
  If clippy or rustc warns about unused imports (e.g., `std::process::Command` if it was only used by `build_typescript`), remove them from the `use` block at the top of `build.rs`.

  Re-run until clean.

- [ ] **Step 5: Run `wafer-cli` tests.**

  Run: `cargo test -p wafer-cli`
  Expected: PASS.

- [ ] **Step 6: Stage and commit.**

  ```bash
  git add crates/wafer-cli/src/build.rs
  git commit -m "$(cat <<'EOF'
  refactor(cli): remove build_typescript and Lang::TypeScript dispatch (Spec 3C)

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 5: Strip TypeScript scaffold from `scaffold.rs`

**Files:**
- Modify: `crates/wafer-cli/src/scaffold.rs`

**Goal:** Remove `Lang::TypeScript` dispatch (line 38), the per-language `println!` block at lines 53-60, the `// TypeScript scaffold` section header at line 182, the entire `fn scaffold_typescript` (line 185-296), and the `const RUNTIME_LIB_RS` raw string (line 302-469). The `with_context` / `pack_ptr_len` / `string_to_packed` items the spec flagged are *inside* `RUNTIME_LIB_RS` (a raw string), so they vanish with the constant — no separate deletion needed.

- [ ] **Step 1: Remove the dispatch arm in `fn scaffold` at line 38.**

  Find this block (lines ~35-39):
  ```rust
      match lang {
          Lang::Rust => scaffold_rust(dir, name, block_name)?,
          Lang::Go => scaffold_go(dir, name, block_name)?,
          Lang::TypeScript => scaffold_typescript(dir, name, block_name)?,
      }
  ```
  Becomes:
  ```rust
      match lang {
          Lang::Rust => scaffold_rust(dir, name, block_name)?,
          Lang::Go => scaffold_go(dir, name, block_name)?,
      }
  ```

- [ ] **Step 2: Remove the `Lang::TypeScript` arm in the `println!` block at lines 53-60.**

  Find this block (lines ~44-61):
  ```rust
      match lang {
          Lang::Rust => {
              println!("  Cargo.toml");
              println!("  src/lib.rs");
          }
          Lang::Go => {
              println!("  go.mod");
              println!("  main.go");
          }
          Lang::TypeScript => {
              println!("  package.json");
              println!("  tsconfig.json");
              println!("  src/index.ts");
              println!("  runtime/Cargo.toml");
              println!("  runtime/src/lib.rs");
              println!("  runtime/bundle.js  (placeholder)");
          }
      }
  ```
  Becomes:
  ```rust
      match lang {
          Lang::Rust => {
              println!("  Cargo.toml");
              println!("  src/lib.rs");
          }
          Lang::Go => {
              println!("  go.mod");
              println!("  main.go");
          }
      }
  ```

- [ ] **Step 3: Remove the `// TypeScript scaffold` section header (line 182) and `fn scaffold_typescript` (line 185 through its closing `}` around line 296).**

  Delete from the comment block:
  ```rust
  // ---------------------------------------------------------------------------
  // TypeScript scaffold
  // ---------------------------------------------------------------------------
  ```
  through and including the closing `}` of `fn scaffold_typescript`.

- [ ] **Step 4: Remove `const RUNTIME_LIB_RS` (line 302 through its terminating `"#;` at line 469).**

  Also remove the `/// The Rust source for the boa_engine-based WASM runtime.` doc comment block (about 4 lines) immediately above the constant.

  After this, the next top-level item should be `fn write_test_fixture` at line 475.

- [ ] **Step 5: Remove any now-unused imports.**

  Run: `cargo check -p wafer-cli 2>&1 | tail -30`
  Expected: clean. If there are unused-import warnings, remove them from the `use` block at the top of `scaffold.rs`.

- [ ] **Step 6: Run `wafer-cli` tests.**

  Run: `cargo test -p wafer-cli`
  Expected: PASS.

- [ ] **Step 7: Stage and commit.**

  ```bash
  git add crates/wafer-cli/src/scaffold.rs
  git commit -m "$(cat <<'EOF'
  refactor(cli): remove scaffold_typescript and embedded boa runtime template (Spec 3C)

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 6: Strip `Lang::TypeScript` from `detect.rs`

**Files:**
- Modify: `crates/wafer-cli/src/detect.rs`

**Goal:** Remove the enum variant, parse arm, detection branch, doc comment, and update the user-facing error message.

- [ ] **Step 1: Replace the file's contents.**

  After the change, `detect.rs` should look exactly like this:

  ```rust
  use std::path::Path;

  use anyhow::bail;

  /// The programming language of a block project.
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Lang {
      Rust,
      Go,
  }

  impl Lang {
      /// Parse a language string from the CLI (case-insensitive).
      pub fn from_str(s: &str) -> anyhow::Result<Self> {
          match s.to_ascii_lowercase().as_str() {
              "rust" | "rs" => Ok(Lang::Rust),
              "go" | "golang" => Ok(Lang::Go),
              other => bail!("Unknown language {other:?}. Supported: rust, go"),
          }
      }
  }

  /// Detect the language of an existing block project in `dir` by inspecting
  /// well-known files.
  #[allow(dead_code)]
  ///
  /// Detection order:
  ///   1. `Cargo.toml`  → Rust
  ///   2. `go.mod`      → Go
  pub fn detect_language(dir: &Path) -> anyhow::Result<Lang> {
      if dir.join("Cargo.toml").exists() {
          return Ok(Lang::Rust);
      }
      if dir.join("go.mod").exists() {
          return Ok(Lang::Go);
      }
      bail!(
          "Could not detect language in {}: no Cargo.toml or go.mod found",
          dir.display()
      )
  }
  ```

  (Note: the `#[allow(dead_code)]` is preserved because it's already on the function — even though `build.rs` does call `detect_language`, removing the attribute is out of scope.)

- [ ] **Step 2: Verify nothing in the workspace still pattern-matches on `Lang::TypeScript`.**

  Run: `cargo check -p wafer-cli 2>&1 | tail -20`
  Expected: PASS.

  If any file (e.g. another match arm we missed in build.rs or scaffold.rs) still references `Lang::TypeScript`, the compile will tell you exactly where. Fix and re-run.

- [ ] **Step 3: Run `wafer-cli` tests.**

  Run: `cargo test -p wafer-cli`
  Expected: PASS.

- [ ] **Step 4: Stage and commit.**

  ```bash
  git add crates/wafer-cli/src/detect.rs
  git commit -m "$(cat <<'EOF'
  refactor(cli): remove Lang::TypeScript variant and detection (Spec 3C)

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 7: Update `wafer new --lang` doc comment

**Files:**
- Modify: `crates/wafer-cli/src/main.rs:40`

**Goal:** Keep the CLI help text honest.

- [ ] **Step 1: Edit the doc comment at line 40.**

  Find:
  ```rust
          /// Programming language: rust | go | typescript.
          #[arg(long, default_value = "rust")]
          lang: String,
  ```
  Replace with:
  ```rust
          /// Programming language: rust | go.
          #[arg(long, default_value = "rust")]
          lang: String,
  ```

- [ ] **Step 2: Verify `--help` text reflects the change.**

  Run: `cargo run -p wafer-cli -- new --help 2>&1 | grep -i 'language\|lang'`
  Expected: shows `Programming language: rust | go.` (no mention of `typescript`).

- [ ] **Step 3: Stage and commit.**

  ```bash
  git add crates/wafer-cli/src/main.rs
  git commit -m "$(cat <<'EOF'
  docs(cli): drop typescript from --lang help text (Spec 3C)

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

### Task 8: Final verification sweep

**Files:** none modified — runs the verification plan from the spec.

- [ ] **Step 1: Run the CI-mirror test command.**

  Run: `cargo test --workspace --exclude wafer-run && cargo test -p wafer-run --lib`
  Expected: all PASS.

- [ ] **Step 2: Run clippy against the whole workspace.**

  Run: `cargo clippy --workspace --all-targets -- -D warnings`
  Expected: clean exit. No new warnings introduced by the cleanup.

- [ ] **Step 3: Run nightly fmt (CI gate).**

  Run: `cargo +nightly fmt --all`
  Expected: no diff. If there is a diff, stage and commit it as a NEW commit:
  ```bash
  git add -u && git commit -m "style: cargo +nightly fmt"
  ```

- [ ] **Step 4: Smoke-test the CLI surface.**

  Use a temp dir to avoid polluting the worktree:
  ```bash
  TMP=$(mktemp -d) && cd "$TMP"
  cargo run --manifest-path /home/joris/Programs/suppers-ai/workspace/wafer-run-js-sdk-cleanup/Cargo.toml -p wafer-cli -- new --lang rust foo/r-block
  cargo run --manifest-path /home/joris/Programs/suppers-ai/workspace/wafer-run-js-sdk-cleanup/Cargo.toml -p wafer-cli -- new --lang go foo/g-block
  cargo run --manifest-path /home/joris/Programs/suppers-ai/workspace/wafer-run-js-sdk-cleanup/Cargo.toml -p wafer-cli -- new --lang ts foo/t-block 2>&1 | tail -5
  cd /home/joris/Programs/suppers-ai/workspace/wafer-run-js-sdk-cleanup && rm -rf "$TMP"
  ```
  Expected:
  - `--lang rust` → succeeds (`Created block project in ./r-block/`).
  - `--lang go` → succeeds.
  - `--lang ts` → fails with `Error: ... Unknown language "ts". Supported: rust, go`.

- [ ] **Step 5: Confirm `npm install` is clean.**

  Run: `npm install 2>&1 | tail -5`
  Expected: completes without error; either `up to date` or modest install output.

- [ ] **Step 6: Sanity-sweep grep — no stragglers in tracked files.**

  Run: `git grep -i 'typescript\|sdks/js\|@wafer-run/sdk\|boa_engine' -- ':!docs/specs/2026-05-01-js-sdk-cleanup-design.md' ':!docs/plans/2026-05-01-js-sdk-cleanup.md'`
  Expected: empty output.

  If anything remains in production code, surface it and resolve before opening the PR. (Hits inside historical specs/plans for *other* features are acceptable; the exclusion above only filters this PR's own docs.)

- [ ] **Step 7: No commit (verification only).**

---

### Task 9: Push and open PR

**Files:** none.

- [ ] **Step 1: Push the branch.**

  Run: `git push -u origin feat/js-sdk-cleanup`
  Expected: branch published. NOTE: the branch was created with `git worktree add --no-track` so it has no upstream; `-u origin feat/js-sdk-cleanup` is required to set one without accidentally tracking `main`.

- [ ] **Step 2: Open PR using `gh pr create`.**

  ```bash
  gh pr create \
    --repo wafer-run/wafer-run \
    --base main \
    --head feat/js-sdk-cleanup \
    --title "refactor: retire JavaScript/TypeScript block subsystem (Spec 3C)" \
    --body "$(cat <<'EOF'
  ## Summary

  Removes the half-implemented JS/TS block subsystem from \`wafer-run\`. Closes the developer-experience hardening initiative honestly: ship nothing rather than a partial runtime that throws on \`callBlock\` / \`log\` / \`isCancelled\` and has no consumers.

  See \`docs/specs/2026-05-01-js-sdk-cleanup-design.md\` for full context. TL;DR:

  - \`@wafer-run/sdk\` was never published to npm.
  - No \`examples/\`, \`docs/\`, or \`README\` mentions TypeScript.
  - The TS build pipeline (\`wafer build\`) is wired end-to-end but the host imports throw at runtime.
  - No tests exercise \`Lang::TypeScript\`.

  ### What changes
  - Deletes the \`sdks/js/\` package and removes it from the npm workspace.
  - Removes \`Lang::TypeScript\` everywhere it appears in \`wafer-cli\` (\`detect.rs\`, \`scaffold.rs\`, \`build.rs\`, \`main.rs\` help text).
  - Removes the boa-only WASI stubs (\`poll_oneoff\`, \`sched_yield\`) from \`validate.rs\`.
  - Regenerates \`package-lock.json\`.

  Net diff: ~600 LOC removed, 0 added (excluding lockfile churn).

  ### Hidden-consumer caveat
  The package was unpublished but a downstream user *could* have depended on it via a git URL. Risk is near-zero; flagging here so it is visible at merge time.

  ## Test plan
  - [ ] CI green (Format & Lint, Tests, Security Audit).
  - [ ] \`cargo test --workspace --exclude wafer-run && cargo test -p wafer-run --lib\` passes locally.
  - [ ] \`cargo clippy --workspace --all-targets -- -D warnings\` clean.
  - [ ] \`wafer new --lang rust\` and \`--lang go\` still scaffold; \`--lang ts\` rejects with \`Supported: rust, go\`.
  - [ ] \`npm install\` succeeds at repo root.
  - [ ] \`git grep -i 'typescript|sdks/js|@wafer-run/sdk|boa_engine'\` is empty in production code.

  🤖 Generated with [Claude Code](https://claude.com/claude-code)
  EOF
  )"
  ```

- [ ] **Step 3: Watch CI.**

  Run: `gh pr checks --repo wafer-run/wafer-run --watch`
  Expected: Format & Lint, Tests, Security Audit all PASS. If any fail, fix at root cause (no `--no-verify`, no skipping hooks) and push a new commit.

---

## Self-review checklist (already run by plan author — recorded for reference)

- **Spec coverage:** every removal item in the spec's manifest maps to a task: sdks/js (T2), validate.rs WASI stubs (T3), build.rs (T4), scaffold.rs (T5), detect.rs (T6), main.rs help text (T7). Verification plan items map to T8 steps.
- **Placeholders:** none. Every step shows exact paths, exact code, and exact commands.
- **Type consistency:** the `Lang` enum's final shape (variants `Rust`, `Go`) is consistent across T6 (the source of truth) and the match-arm edits in T4 and T5.
- **Find_wasm_in_dir correction:** during planning, verified the function is called by both Rust (build.rs:130) and TS (build.rs:247) paths. T4 explicitly preserves it.
- **Helper-fn correction:** spec was conservative about `with_context` / `pack_ptr_len` / `string_to_packed` in scaffold.rs. Verified during planning: those names appear only inside the `RUNTIME_LIB_RS` raw string. They vanish with the constant; T5 makes that explicit.
- **TDD note:** this is a deletion PR; there is nothing new to test-drive. The verification structure (T1 baseline, T8 final sweep) replaces the usual TDD loop.
