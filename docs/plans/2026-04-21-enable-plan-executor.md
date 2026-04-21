# Enable Autonomous Plan Executor for wafer-run

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `wafer-run` dispatch-eligible by the workspace-level plan executor. Add `scripts/check.sh` (single definition of green, reused by CI and autonomous runs) and append the Autonomous Execution Principles block to `CLAUDE.md`.

**Architecture:** One PR in `wafer-run` adds two files (`scripts/check.sh`, appended block in `CLAUDE.md`) and this plan file itself. `scripts/check.sh` mirrors the existing `.github/workflows/ci.yml` commands exactly.

**Target repo:** `wafer-run` (`github.com/wafer-run/wafer-run`). All commits go on branch `chore/enable-plan-executor`, branched from `origin/main`. Merged via PR (do not push to main directly; do not self-merge).

**Dependencies:** Task 5 (end-to-end smoke test) requires the workspace-level companion plan `docs/superpowers/plans/2026-04-21-autonomous-plan-executor.md` to be completed first so that `bin/dispatch-plan` exists. Tasks 1–4 can run independently.

---

### Task 1: Create bootstrap branch and commit this plan

**Files:**
- Commit (untracked): `docs/plans/2026-04-21-enable-plan-executor.md`

- [ ] **Step 1: Fetch and create branch from origin/main**

```bash
cd /home/joris/Programs/suppers-ai/workspace/wafer-run
git fetch origin main
git switch -c chore/enable-plan-executor origin/main
```

Verify:
```bash
git branch --show-current
# expected: chore/enable-plan-executor

git log -1 --format='%H' HEAD
git log -1 --format='%H' origin/main
# expected: identical
```

- [ ] **Step 2: Commit the plan file**

This plan file should already exist in the working tree as an untracked file (it was written before Task 1). Stage and commit:
```bash
git add docs/plans/2026-04-21-enable-plan-executor.md
git commit -m "docs(plan): enable autonomous plan executor for wafer-run"
```

---

### Task 2: Add `scripts/check.sh`

**Files:**
- Create: `scripts/check.sh`

Mirrors `.github/workflows/ci.yml`:
- Format: `cargo +nightly fmt --all -- --check`
- Clippy: `cargo clippy --workspace -- -D warnings`
- Tests: `cargo test --workspace --exclude wafer-run && cargo test -p wafer-run --lib`

- [ ] **Step 1: Write the script**

Create `scripts/check.sh`:
```bash
#!/usr/bin/env bash
# Single definition of "green" for wafer-run.
# Used by CI, by the autonomous plan executor, and by local dev.
#
# MUST stay in sync with .github/workflows/ci.yml; if one changes, the other
# must change too.
#
# Usage: ./scripts/check.sh
set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> Format (nightly rustfmt)"
cargo +nightly fmt --all -- --check

echo "==> Clippy"
cargo clippy --workspace -- -D warnings

echo "==> Tests (workspace except wafer-run, then wafer-run --lib)"
cargo test --workspace --exclude wafer-run
cargo test -p wafer-run --lib

echo "==> All checks passed."
```

- [ ] **Step 2: Make executable**

```bash
chmod +x scripts/check.sh
```

- [ ] **Step 3: Run it to verify current main is clean**

```bash
./scripts/check.sh
```

Expected: all four phases print, all pass, final line is `==> All checks passed.`

If any step fails, that's a pre-existing `wafer-run` issue. Do **not** weaken the script. Fix the underlying problem in a separate PR first, then return to this plan.

- [ ] **Step 4: Commit**

```bash
git add scripts/check.sh
git commit -m "chore: add scripts/check.sh mirroring CI"
```

---

### Task 3: Add Autonomous Execution Principles to `CLAUDE.md`

**Files:**
- Modify: `CLAUDE.md`

The existing `CLAUDE.md` has a few development bullets; the new block goes at the end of the file as its own `## Autonomous Execution Principles` section. The first existing bullet ("Always fix the real issue...") overlaps with the new "Code quality" section; leave it in place — redundancy here is cheap and the existing wording is already internalized.

- [ ] **Step 1: Append the principles block**

Append to the end of `CLAUDE.md`:
```markdown

## Autonomous Execution Principles

These apply to all Claude sessions in this repo, especially autonomous
runs dispatched via `<workspace>/.claude-runs/`.

**Code quality**
- Fix at the root cause. Do not patch symptoms, add compat shims, or
  introduce feature flags to paper over bugs.
- No quick fixes or TODO-style placeholders in shipped code.
- No hardcoded domain values — use config, constants, or registry as the
  repo already does.
- Follow existing patterns before introducing new ones. Read the
  surrounding code first.

**Scope discipline**
- Stay inside the plan. Do not refactor unrelated code.
- Do not add speculative abstractions for future requirements.
- Delete dead code rather than leaving `_unused` aliases or `// removed`
  notes.

**When blocked**
- If genuinely unsure which of two interpretations is correct, stop.
- Do not guess. Write the question to `.claude-run/STATUS.md` and open the
  PR as draft with `## Blockers` filled in.

**Git boundaries**
- Do not push to `main`. Ever.
- Do not merge any pull request, including your own.
- Do not force-push or delete branches you did not create in this run.
- Your only job is to open the PR. A human merges it.

**Verification**
- `./scripts/check.sh` is the single definition of green. It must pass
  before opening a non-draft PR.
```

- [ ] **Step 2: Verify**

```bash
grep -c '^## ' CLAUDE.md
# expected: 1 (the new heading)

tail -25 CLAUDE.md
# expected: Verification bullet at the bottom
```

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: add Autonomous Execution Principles to CLAUDE.md"
```

---

### Task 4: Push branch and open PR

**Files:** none

- [ ] **Step 1: Push branch**

```bash
git push -u origin chore/enable-plan-executor
```

- [ ] **Step 2: Open PR (ready-for-review)**

```bash
gh pr create \
  --base main \
  --head chore/enable-plan-executor \
  --title "Enable autonomous plan executor for wafer-run" \
  --body "$(cat <<'EOF'
## Summary
- Adds `scripts/check.sh` mirroring CI commands (single definition of green for CI + autonomous runs + local dev).
- Appends Autonomous Execution Principles block to `CLAUDE.md` (code quality, scope discipline, when-blocked, git boundaries, verification).
- Commits the plan document that drove this change.

Makes `wafer-run` dispatch-eligible by `bin/dispatch-plan` in the workspace.

## Test plan
- [x] `./scripts/check.sh` runs clean on this branch.
- [ ] After merge: dispatch a trivial plan via `bin/dispatch-plan` and verify the container produces a ready-for-review PR (see plan Task 5).
EOF
)"
```

- [ ] **Step 3: Record PR number for Task 5**

The PR URL from `gh pr create` output includes the number. Save it for use in Task 5.

- [ ] **Step 4: STOP. Wait for human to merge.**

The human reviews the PR and merges. Do not self-merge. Do not continue to Task 5 until `main` contains this PR's commits.

---

### Task 5: Post-merge end-to-end smoke test

Runs on the host after Task 4's PR has been merged and the workspace plan (`docs/superpowers/plans/2026-04-21-autonomous-plan-executor.md`) has been completed.

**Files:**
- Create (temporary, in wafer-run): `docs/plans/2026-04-21-dispatcher-smoke.md`

- [ ] **Step 1: Ensure prerequisites**

From workspace root:
```bash
cd /home/joris/Programs/suppers-ai/workspace

# bin/dispatch-plan exists and is executable?
test -x bin/dispatch-plan && echo "dispatch-plan: OK"

# wafer-run/main now has scripts/check.sh?
( cd wafer-run && git fetch origin main && git ls-tree origin/main scripts/check.sh ) \
    && echo "wafer-run scripts/check.sh on main: OK"

# Devcontainer running?
docker ps --filter "label=devcontainer.local_folder=$(pwd)" --format '{{.Names}}'
# expected: one running container name
```

- [ ] **Step 2: Author a trivial smoke plan in wafer-run**

In the wafer-run working tree on a fresh branch from origin/main (NOT on `chore/enable-plan-executor`, which is already merged):
```bash
cd /home/joris/Programs/suppers-ai/workspace/wafer-run
git fetch origin main
git switch main && git pull --ff-only
# (author the plan file on main locally; the dispatcher will branch from origin/main for the actual run)
```

Create `wafer-run/docs/plans/2026-04-21-dispatcher-smoke.md`:
```markdown
# Dispatcher Smoke Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans

**Goal:** Verify the autonomous plan executor end-to-end by making one no-op documented edit.

### Task 1: Add a smoke marker

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Append one line to the end of `CLAUDE.md`**

Append:
```
<!-- dispatcher-smoke: 2026-04-21 -->
```

- [ ] **Step 2: Run `./scripts/check.sh`**

Expected: all checks pass.

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "chore: dispatcher-smoke marker"
```
```

Commit this plan file on `main` locally (no push, no PR — it just needs to exist in the worktree the dispatcher branches from):

Actually the dispatcher branches from `origin/main`, so the plan file needs to be on `origin/main` for the container to see it. That requires a second PR for the smoke plan itself, which is overkill.

**Better approach:** the dispatcher reads the plan from the host's copy (the `cp "$PLAN_PATH" "$RUN_DIR/.claude-run/plan.md"` step). So the plan file just needs to exist on the host's wafer-run working tree at dispatch time. It does NOT need to be committed to `origin/main`. The container sees the plan via the sidecar copy, not via the worktree checkout.

So after writing `docs/plans/2026-04-21-dispatcher-smoke.md` in wafer-run's working tree, leave it as an untracked file. Do not commit.

- [ ] **Step 3: Dispatch the smoke plan**

```bash
cd /home/joris/Programs/suppers-ai/workspace
./bin/dispatch-plan wafer-run/docs/plans/2026-04-21-dispatcher-smoke.md
```

Expected: `preflights: OK ...`, worktree + branch + prompt paths printed, `Launched.` footer.

- [ ] **Step 4: Observe**

```bash
tail -f .claude-runs/wafer-run/2026-04-21-dispatcher-smoke/.claude-run/run.log
```

Wait for completion (a few minutes).

```bash
cat .claude-runs/wafer-run/2026-04-21-dispatcher-smoke/.claude-run/STATUS.md
```

Expected: `status: succeeded` with a PR URL.

- [ ] **Step 5: Inspect the PR**

```bash
gh pr view <pr-number> --repo wafer-run/wafer-run --json title,body,state,isDraft,files
```

Expected:
- `state: OPEN`
- `isDraft: false`
- `files`: one-line change to `CLAUDE.md`

- [ ] **Step 6: Teardown**

Close the PR without merging, delete the branch, delete the run dir, delete the untracked smoke plan file:
```bash
gh pr close <pr-number> --delete-branch --repo wafer-run/wafer-run

cd /home/joris/Programs/suppers-ai/workspace
( cd wafer-run && git worktree remove -f "$(pwd)/../.claude-runs/wafer-run/2026-04-21-dispatcher-smoke/worktree" 2>/dev/null ) || true
( cd wafer-run && git branch -D claude-run/2026-04-21-dispatcher-smoke 2>/dev/null ) || true
rm -rf .claude-runs/wafer-run/2026-04-21-dispatcher-smoke
rm wafer-run/docs/plans/2026-04-21-dispatcher-smoke.md
```

- [ ] **Step 7: No commit**

Nothing from Task 5 is committed. The smoke PR was the artifact and is closed.

---

## Self-review checklist (for the plan author only)

- Each task commits inside `wafer-run` on branch `chore/enable-plan-executor` (Tasks 1–3), pushes + opens PR on branch (Task 4), or operates on the host post-merge (Task 5).
- The branch is explicitly branched from `origin/main` (not from `feat/llm-support-additions` which wafer-run currently sits on).
- Task 4 stops before merge; the human merges.
- Task 5 is purely verification: nothing is committed; the smoke PR is torn down.
