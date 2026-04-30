# wafer-run Spec 3B — `wafer dev` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `wafer dev` subcommand to `wafer-cli` that watches files, kills+respawns a `cargo run` child on changes, and prints a wafer-aware boot summary on each successful start. Plus add three structured tracing events (`wafer.runtime.starting/flow_registered/listening`) to the runtime so the boot summary has a stable signal to parse.

**Architecture:** All new CLI code in `crates/wafer-cli/src/commands/dev/` — five focused modules: `mod.rs` (entrypoint + clap), `bin_detect.rs`, `watcher.rs`, `supervisor.rs`, `summary.rs`. Three small runtime additions: one `tracing::info!` in `Wafer::start`, one in `Wafer::add_flow_json`, restructure the existing http-listener log to use the structured target. New deps: `notify`, `notify-debouncer-mini`. No changes to flow definitions, examples, or block crates.

**Tech Stack:** Rust, `clap` (subcommand), `tokio` (process supervision via `tokio::process::Child`), `notify` + `notify-debouncer-mini` (file watching), `tracing` (boot signal contract), `regex` for parsing tracing output.

**Spec:** `docs/specs/2026-04-30-wafer-dev-command-design.md`

---

## File map

Files this plan creates or modifies (all in `/home/joris/Programs/suppers-ai/workspace/wafer-run/`):

**Runtime — structured tracing events:**
- Modify: `crates/wafer-run/src/runtime/lifecycle.rs` — add `event = "starting"` info log at top of `Wafer::start`.
- Modify: `crates/wafer-run/src/runtime/registry.rs` — add `event = "flow_registered"` info log at end of `Wafer::add_flow_json`.
- Modify: `crates/wafer-block-http-listener/src/lib.rs` — restructure existing "listening on" line to use `target = "wafer.runtime"` + structured fields.

**CLI — new dev subcommand:**
- Create: `crates/wafer-cli/src/commands/dev/mod.rs` — clap config, entry function, glues submodules together.
- Create: `crates/wafer-cli/src/commands/dev/bin_detect.rs` — Cargo.toml inspection to auto-detect bin target.
- Create: `crates/wafer-cli/src/commands/dev/watcher.rs` — notify wiring, debouncer, default + user pattern merging.
- Create: `crates/wafer-cli/src/commands/dev/supervisor.rs` — process state machine, kill cascade.
- Create: `crates/wafer-cli/src/commands/dev/summary.rs` — tracing-line parser, banner formatter.
- Modify: `crates/wafer-cli/src/main.rs` — register `Commands::Dev { … }` variant + dispatch.
- Modify: `crates/wafer-cli/src/commands/mod.rs` — `pub mod dev;`.
- Modify: `crates/wafer-cli/Cargo.toml` — add `notify`, `notify-debouncer-mini`, `regex` deps.

**Tests:**
- Create: `crates/wafer-cli/src/commands/dev/summary.rs` — inline `#[cfg(test)] mod tests { … }`.
- Create: `crates/wafer-cli/src/commands/dev/bin_detect.rs` — inline `#[cfg(test)] mod tests { … }`.
- Create: `crates/wafer-cli/src/commands/dev/watcher.rs` — inline `#[cfg(test)] mod tests { … }`.
- Create: `crates/wafer-cli/tests/dev.rs` — integration test against a fake-app fixture.
- Create: `crates/wafer-cli/tests/fixtures/dev-fake-app/Cargo.toml`
- Create: `crates/wafer-cli/tests/fixtures/dev-fake-app/src/main.rs`

**Documentation:**
- Modify: `/home/joris/Programs/suppers-ai/workspace/site/content/docs/cli.html` — add `wafer dev` to the CLI command reference.
- Modify: `crates/wafer-cli/src/main.rs` — `--help` description for the new subcommand.

---

### Task 1: Create feature branch in wafer-run

**Files:** none

- [ ] **Step 1: Verify clean main and create branch**

```bash
cd /home/joris/Programs/suppers-ai/workspace/wafer-run
git status -s                       # should show docs/specs/2026-04-30-wafer-dev-command-design.md and docs/plans/2026-04-30-wafer-dev-command.md as untracked
git checkout main
git pull --ff-only
git checkout -b feat/wafer-dev
git branch -vv | grep feat/wafer-dev
```

Expected:
- After branch creation: `On branch feat/wafer-dev`. `git branch -vv` shows the new branch with NO upstream tracking annotation.

⚠️ **Do NOT use `git checkout -b feat/wafer-dev origin/main`** — that sets upstream to `origin/main` and a later `git push` pushes to `main` directly. Plain `-b feat/wafer-dev` (no second argument) is correct.

- [ ] **Step 2: Commit spec + plan as the first commit**

```bash
git add docs/specs/2026-04-30-wafer-dev-command-design.md docs/plans/2026-04-30-wafer-dev-command.md
git commit -m "docs(spec): add Spec 3B design + plan for wafer dev command"
git log --oneline -1
```

---

### Task 2: Add structured tracing events to the runtime

**Files:**
- Modify: `crates/wafer-run/src/runtime/lifecycle.rs`
- Modify: `crates/wafer-run/src/runtime/registry.rs`
- Modify: `crates/wafer-block-http-listener/src/lib.rs`

This task adds the three events `wafer dev`'s summary parser depends on. Lands first so the contract is in place before the consumer is written.

- [ ] **Step 1: Add `event = "starting"` to `Wafer::start`**

Open `crates/wafer-run/src/runtime/lifecycle.rs`. Find the `pub async fn start` method (around line 96 in current main; the exact line number may drift — locate by signature). It begins:

```rust
pub async fn start(mut self) -> Result<Arc<Self>, RuntimeError> {
    self.start_without_bind().await?;

    for (name, block) in &self.blocks {
        ...
```

Insert one line before `self.start_without_bind().await?`:

```rust
pub async fn start(mut self) -> Result<Arc<Self>, RuntimeError> {
    tracing::info!(
        target: "wafer.runtime",
        event = "starting",
        blocks = self.blocks.len(),
        "wafer runtime starting"
    );
    self.start_without_bind().await?;

    for (name, block) in &self.blocks {
        ...
```

(`self.blocks` is the `HashMap<String, Arc<dyn Block>>` field on `Wafer`. `.len()` is the registered-block count at start time, before resolution adds any deferred blocks.)

- [ ] **Step 2: Add `event = "flow_registered"` to `Wafer::add_flow_json`**

Open `crates/wafer-run/src/runtime/registry.rs`. Find `pub fn add_flow_json` (around line 169 in current main). It currently ends:

```rust
        self.add_flow(flow);
        Ok(())
    }
```

Replace those three lines with:

```rust
        let flow_id = flow.id.clone();
        self.add_flow(flow);
        tracing::info!(
            target: "wafer.runtime",
            event = "flow_registered",
            flow = %flow_id,
            "registered flow"
        );
        Ok(())
    }
```

(Capture `flow.id` BEFORE `add_flow` consumes the value. The `%` sigil uses `Display` formatting; `flow.id` is `String`.)

- [ ] **Step 3: Restructure http-listener "listening" log**

Open `crates/wafer-block-http-listener/src/lib.rs`. Find the existing line (around line 429):

```rust
tracing::info!("wafer-run/http-listener listening on {}", listen);
```

Replace with:

```rust
tracing::info!(
    target: "wafer.runtime",
    event = "listening",
    addr = %listen,
    "wafer-run/http-listener listening"
);
```

(`listen` is the bind address `String` already in scope — verify its name in current code; if different, use whatever the local variable is called. The only requirement is the `target`, `event`, and `addr` field names.)

- [ ] **Step 4: Verify the runtime still builds and tests still pass**

```bash
cargo build --workspace 2>&1 | tail -3
cargo test --workspace --exclude wafer-run 2>&1 | tail -5
cargo test -p wafer-run --lib 2>&1 | tail -5
```

Expected: builds clean, all tests pass. No tests should observe these specific log lines (they're new); if any test asserts on the OLD free-form "listening on" string, update it to match the new structured format.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-run/src/runtime/lifecycle.rs crates/wafer-run/src/runtime/registry.rs crates/wafer-block-http-listener/src/lib.rs
git commit -m "feat(runtime): emit structured wafer.runtime tracing events for dev tooling"
```

---

### Task 3: Add CLI scaffolding and bin auto-detection

**Files:**
- Modify: `crates/wafer-cli/Cargo.toml`
- Create: `crates/wafer-cli/src/commands/dev/mod.rs`
- Create: `crates/wafer-cli/src/commands/dev/bin_detect.rs`
- Modify: `crates/wafer-cli/src/commands/mod.rs`
- Modify: `crates/wafer-cli/src/main.rs`

- [ ] **Step 1: Add new dependencies**

Open `crates/wafer-cli/Cargo.toml`. Find the `[dependencies]` section. Add these three entries (alphabetize within the section per the existing convention):

```toml
notify = "6"
notify-debouncer-mini = "0.4"
regex = "1"
```

If `regex` or any of these are already present, don't duplicate.

- [ ] **Step 2: Register the dev module**

Open `crates/wafer-cli/src/commands/mod.rs`. Add `pub mod dev;` alongside the existing `pub mod search;` etc.

- [ ] **Step 3: Write the failing test for bin auto-detection**

Create `crates/wafer-cli/src/commands/dev/bin_detect.rs`:

```rust
//! Cargo.toml inspection: figure out which binary `wafer dev` should run.

use std::path::Path;

use anyhow::{anyhow, bail, Result};

/// Outcome of binary detection.
pub enum DetectedBin {
    /// Exactly one bin candidate; use this name.
    One(String),
    /// Multiple bins; the user must pass `--bin`.
    Multiple(Vec<String>),
    /// No bin found at all.
    None,
}

/// Inspect a Cargo.toml at `manifest_path` and report what bin targets exist.
///
/// Considers explicit `[[bin]]` entries plus the implicit `src/main.rs` (whose
/// bin name is the package name).
pub fn detect_bins(manifest_path: &Path) -> Result<DetectedBin> {
    let toml_text = std::fs::read_to_string(manifest_path)
        .map_err(|e| anyhow!("failed to read {}: {e}", manifest_path.display()))?;
    detect_bins_from_str(
        &toml_text,
        manifest_path
            .parent()
            .ok_or_else(|| anyhow!("Cargo.toml has no parent dir"))?,
    )
}

pub(crate) fn detect_bins_from_str(toml_text: &str, crate_dir: &Path) -> Result<DetectedBin> {
    let doc: toml::Value = toml::from_str(toml_text)
        .map_err(|e| anyhow!("Cargo.toml is not valid TOML: {e}"))?;

    let mut bins: Vec<String> = Vec::new();

    // Explicit [[bin]] entries
    if let Some(bin_array) = doc.get("bin").and_then(|v| v.as_array()) {
        for entry in bin_array {
            if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
                bins.push(name.to_string());
            }
        }
    }

    // Implicit src/main.rs → bin named after the package
    if crate_dir.join("src/main.rs").exists() {
        if let Some(pkg_name) = doc
            .get("package")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
        {
            if !bins.iter().any(|b| b == pkg_name) {
                bins.push(pkg_name.to_string());
            }
        }
    }

    Ok(match bins.len() {
        0 => DetectedBin::None,
        1 => DetectedBin::One(bins.into_iter().next().unwrap()),
        _ => DetectedBin::Multiple(bins),
    })
}

/// Resolve `--bin` argument against detection result. Errors with a friendly
/// message if the user needs to disambiguate or if no bin was found.
pub fn resolve_bin(detected: DetectedBin, user_bin: Option<&str>) -> Result<String> {
    match (detected, user_bin) {
        (_, Some(name)) => Ok(name.to_string()),
        (DetectedBin::One(name), None) => Ok(name),
        (DetectedBin::Multiple(names), None) => {
            bail!(
                "multiple bin targets found ({}); pass --bin <name>",
                names.join(", ")
            )
        }
        (DetectedBin::None, None) => bail!(
            "no bin target found in Cargo.toml — wafer dev requires a [[bin]] crate or src/main.rs"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fake_dir() -> PathBuf {
        // Returns a path that won't exist; tests don't touch the FS for the
        // src/main.rs check unless explicitly arranged.
        PathBuf::from("/nonexistent-test-dir")
    }

    #[test]
    fn explicit_single_bin_is_one() {
        let toml = r#"
            [package]
            name = "demo"
            [[bin]]
            name = "demo-bin"
        "#;
        match detect_bins_from_str(toml, &fake_dir()).unwrap() {
            DetectedBin::One(n) => assert_eq!(n, "demo-bin"),
            _ => panic!("expected One"),
        }
    }

    #[test]
    fn explicit_multiple_bins_is_multiple() {
        let toml = r#"
            [package]
            name = "demo"
            [[bin]]
            name = "a"
            [[bin]]
            name = "b"
        "#;
        match detect_bins_from_str(toml, &fake_dir()).unwrap() {
            DetectedBin::Multiple(ns) => assert_eq!(ns, vec!["a", "b"]),
            _ => panic!("expected Multiple"),
        }
    }

    #[test]
    fn no_bins_no_main_is_none() {
        let toml = r#"
            [package]
            name = "demo"
        "#;
        match detect_bins_from_str(toml, &fake_dir()).unwrap() {
            DetectedBin::None => {}
            _ => panic!("expected None"),
        }
    }

    #[test]
    fn resolve_user_override_wins() {
        let detected = DetectedBin::Multiple(vec!["a".into(), "b".into()]);
        let resolved = resolve_bin(detected, Some("b")).unwrap();
        assert_eq!(resolved, "b");
    }

    #[test]
    fn resolve_multiple_without_arg_errors() {
        let detected = DetectedBin::Multiple(vec!["a".into(), "b".into()]);
        let err = resolve_bin(detected, None).unwrap_err().to_string();
        assert!(err.contains("multiple bin targets"));
        assert!(err.contains("a, b"));
    }

    #[test]
    fn resolve_none_without_arg_errors() {
        let err = resolve_bin(DetectedBin::None, None).unwrap_err().to_string();
        assert!(err.contains("no bin target found"));
    }
}
```

Note: this introduces `toml` as a dependency. Check whether `wafer-cli` already has `toml` (the `wafer_toml.rs` module uses `toml_edit`). If only `toml_edit` is present, add `toml = "0.8"` (or whatever workspace pin) to `Cargo.toml` as well in Step 1.

- [ ] **Step 4: Run the bin-detection tests to verify they pass**

```bash
cargo test -p wafer-cli commands::dev::bin_detect:: -- --nocapture
```

Expected: 6 tests pass.

- [ ] **Step 5: Add the empty `dev/mod.rs` skeleton**

Create `crates/wafer-cli/src/commands/dev/mod.rs`:

```rust
//! `wafer dev` — file-watching dev loop with wafer-aware boot summary.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::Args;

mod bin_detect;
// More submodules added in subsequent tasks: watcher, supervisor, summary.

/// Arguments for the `wafer dev` subcommand.
#[derive(Args, Debug, Clone)]
pub struct DevArgs {
    /// Cargo bin target to run. If omitted, auto-detected from Cargo.toml.
    #[arg(long)]
    pub bin: Option<String>,

    /// Pass --release to cargo (slower rebuilds, sometimes useful for perf).
    #[arg(long)]
    pub release: bool,

    /// Add an extra path/glob to watch. Repeatable.
    #[arg(long = "watch", value_name = "PATTERN")]
    pub watch: Vec<String>,

    /// Disable the default watch list (src/**/*.rs, Cargo.toml, wafer.lock).
    #[arg(long)]
    pub no_default_watch: bool,

    /// File-change debounce window in milliseconds.
    #[arg(long, default_value_t = 200)]
    pub debounce: u64,

    /// SIGTERM → SIGKILL grace period in seconds.
    #[arg(long, default_value_t = 3)]
    pub kill_timeout: u64,

    /// Extra args forwarded verbatim to cargo run (after `--`).
    #[arg(last = true)]
    pub cargo_args: Vec<String>,
}

/// Entry point invoked by `main.rs`.
pub async fn run(args: DevArgs) -> Result<()> {
    let manifest = PathBuf::from("Cargo.toml");
    let detected = bin_detect::detect_bins(&manifest)?;
    let _bin = bin_detect::resolve_bin(detected, args.bin.as_deref())?;
    let _debounce = Duration::from_millis(args.debounce);

    // Watcher + supervisor + summary wired in subsequent tasks.
    anyhow::bail!("wafer dev: not yet implemented (Task 3 skeleton; tasks 4-6 wire watcher/supervisor/summary)")
}
```

- [ ] **Step 6: Wire the subcommand into `main.rs`**

Open `crates/wafer-cli/src/main.rs`. Add the `Dev` variant to the `Commands` enum (after `Install`):

```rust
    /// Run the wafer-run app in this directory with file-watch + restart on save.
    ///
    /// Reads Cargo.toml from cwd, picks a bin target (or use --bin), runs
    /// `cargo run`, watches Rust source + Cargo.toml + wafer.lock, restarts
    /// on changes. Prints a wafer-aware boot summary on each successful start.
    Dev(commands::dev::DevArgs),
```

In the match block at the bottom of `main.rs`, add:

```rust
        Commands::Dev(args) => {
            commands::dev::run(args).await?;
        }
```

(Place it alphabetically among the existing `Commands::*` arms.)

- [ ] **Step 7: Verify the CLI compiles and `--help` shows the new subcommand**

```bash
cargo build -p wafer-cli 2>&1 | tail -3
cargo run -p wafer-cli -- --help 2>&1 | grep -E '^\s+(dev|new|build|search)' | sort
cargo run -p wafer-cli -- dev --help 2>&1 | head -20
```

Expected: build clean, `dev` listed in subcommands, `dev --help` shows all the flags from `DevArgs`.

- [ ] **Step 8: Commit**

```bash
git add crates/wafer-cli/Cargo.toml crates/wafer-cli/src/commands/mod.rs crates/wafer-cli/src/commands/dev/ crates/wafer-cli/src/main.rs
git commit -m "feat(cli): add wafer dev subcommand skeleton + bin auto-detection"
```

---

### Task 4: File watcher with debounce

**Files:**
- Create: `crates/wafer-cli/src/commands/dev/watcher.rs`
- Modify: `crates/wafer-cli/src/commands/dev/mod.rs`

- [ ] **Step 1: Write failing tests for pattern merging**

Create `crates/wafer-cli/src/commands/dev/watcher.rs`:

```rust
//! File-system watching with debounce. Bridges notify's sync mpsc to a tokio
//! mpsc so the supervisor can `await` change events.

use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebouncedEvent};
use tokio::sync::mpsc;

/// Default watch patterns relative to cwd.
const DEFAULT_PATTERNS: &[&str] = &["src", "Cargo.toml", "wafer.lock"];

/// Compute the effective watch list given user-provided patterns and the
/// `--no-default-watch` flag.
pub fn merge_patterns(user: &[String], use_defaults: bool) -> Vec<String> {
    let mut out: Vec<String> = if use_defaults {
        DEFAULT_PATTERNS.iter().map(|s| s.to_string()).collect()
    } else {
        Vec::new()
    };
    for u in user {
        if !out.iter().any(|p| p == u) {
            out.push(u.clone());
        }
    }
    out
}

/// Spawn a watcher in a background thread; events are forwarded to the
/// returned tokio receiver.
///
/// Patterns that don't exist on disk are skipped with a warning (e.g.
/// `wafer.lock` is absent for non-CLI apps). Patterns that DO exist are
/// watched recursively if they're directories, non-recursively if files.
pub fn spawn_watcher(
    patterns: &[String],
    debounce: Duration,
) -> Result<mpsc::Receiver<()>> {
    let (sync_tx, sync_rx) = std_mpsc::channel::<notify::Result<Vec<DebouncedEvent>>>();
    let (async_tx, async_rx) = mpsc::channel::<()>(8);

    let mut debouncer =
        new_debouncer(debounce, move |res| sync_tx.send(res).expect("watcher rx dropped"))
            .context("failed to construct file-system debouncer")?;

    let mut watched_any = false;
    for pat in patterns {
        let path = Path::new(pat);
        if !path.exists() {
            tracing::warn!(pattern = %pat, "watch pattern does not exist on disk; skipping");
            continue;
        }
        let mode = if path.is_dir() {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        debouncer
            .watcher()
            .watch(path, mode)
            .with_context(|| format!("failed to watch {}", path.display()))?;
        watched_any = true;
    }
    if !watched_any {
        anyhow::bail!("no watch patterns existed on disk; nothing to watch");
    }

    // Bridge thread: forward each debounced batch as a single tokio event.
    std::thread::spawn(move || {
        // Hold the debouncer alive for the lifetime of the bridge thread; if
        // we drop it, notify stops emitting events.
        let _debouncer = debouncer;
        while let Ok(res) = sync_rx.recv() {
            match res {
                Ok(events) if !events.is_empty() => {
                    let _ = async_tx.blocking_send(());
                    let _ = events; // events themselves aren't needed downstream — just the signal
                }
                Ok(_) => {} // empty batch, ignore
                Err(e) => {
                    tracing::warn!(error = ?e, "watcher error (continuing)");
                }
            }
        }
    });

    // Bridge: keep the resource. Move closure took ownership of `debouncer`
    // already, so this point is unreachable — just keep the silencer.
    let _ = PathBuf::new(); // satisfy "use" of PathBuf if unused elsewhere

    Ok(async_rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_alone() {
        let patterns = merge_patterns(&[], true);
        assert_eq!(patterns, vec!["src", "Cargo.toml", "wafer.lock"]);
    }

    #[test]
    fn user_alone_when_no_defaults() {
        let patterns = merge_patterns(&["foo".into(), "bar".into()], false);
        assert_eq!(patterns, vec!["foo", "bar"]);
    }

    #[test]
    fn defaults_plus_user_no_dupes() {
        let patterns = merge_patterns(&["src".into(), "extra".into()], true);
        assert_eq!(patterns, vec!["src", "Cargo.toml", "wafer.lock", "extra"]);
    }

    #[test]
    fn no_defaults_no_user_is_empty() {
        let patterns = merge_patterns(&[], false);
        assert!(patterns.is_empty());
    }
}
```

Note on the bridge thread: the `_debouncer` move-into-thread keeps the debouncer alive. The `let _ = PathBuf::new();` line at the end of `spawn_watcher` is a leftover scaffold — remove it before compiling. (Replace with: just delete that line; the function returns `Ok(async_rx)` directly.)

- [ ] **Step 2: Verify the helper tests pass**

```bash
cargo test -p wafer-cli commands::dev::watcher::tests:: -- --nocapture
```

Expected: 4 tests pass.

- [ ] **Step 3: Wire the watcher into `mod.rs`**

Open `crates/wafer-cli/src/commands/dev/mod.rs`. Add `mod watcher;` near the other `mod` declarations. Replace the placeholder `bail!` in `run()` with:

```rust
pub async fn run(args: DevArgs) -> Result<()> {
    let manifest = PathBuf::from("Cargo.toml");
    let detected = bin_detect::detect_bins(&manifest)?;
    let bin = bin_detect::resolve_bin(detected, args.bin.as_deref())?;
    let debounce = Duration::from_millis(args.debounce);

    let patterns = watcher::merge_patterns(&args.watch, !args.no_default_watch);
    let mut change_rx = watcher::spawn_watcher(&patterns, debounce)?;

    tracing::info!(
        bin = %bin,
        patterns = ?patterns,
        debounce_ms = args.debounce,
        "wafer dev starting (supervisor pending — Task 5)"
    );

    // Drain a few events to confirm the watcher is alive — replaced by the
    // real supervisor loop in Task 5.
    while let Some(()) = change_rx.recv().await {
        tracing::info!("change detected (no-op until Task 5)");
    }

    Ok(())
}
```

- [ ] **Step 4: Smoke-test interactively**

```bash
# In a wafer-app dir (e.g. examples/hello-world after copying), run:
cargo run -p wafer-cli -- dev &
DEV_PID=$!
sleep 2
touch examples/hello-world/src/main.rs
sleep 2
kill $DEV_PID
```

Expected: log output shows "wafer dev starting" and at least one "change detected" line. (This step is exploratory — not a passing/failing assertion. The real integration test lands in Task 7.)

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-cli/src/commands/dev/mod.rs crates/wafer-cli/src/commands/dev/watcher.rs
git commit -m "feat(cli): wire file watcher with debounce into wafer dev"
```

---

### Task 5: Process supervisor + kill cascade

**Files:**
- Create: `crates/wafer-cli/src/commands/dev/supervisor.rs`
- Modify: `crates/wafer-cli/src/commands/dev/mod.rs`

- [ ] **Step 1: Write the supervisor skeleton**

Create `crates/wafer-cli/src/commands/dev/supervisor.rs`:

```rust
//! Process state machine for `wafer dev`. Owns one cargo-run child at a time.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

/// Single restart event from the watcher.
#[derive(Debug, Clone, Copy)]
pub struct Restart;

/// Input to `Supervisor::run`.
pub struct SupervisorConfig {
    pub bin: String,
    pub release: bool,
    pub cargo_args: Vec<String>,
    pub kill_timeout: Duration,
}

/// Drives the supervisor loop: build → spawn → run → kill on change → repeat.
///
/// Returns when `change_rx` is closed (the watcher dropped), which only
/// happens on Ctrl-C / shutdown.
pub async fn run(
    cfg: SupervisorConfig,
    mut change_rx: mpsc::Receiver<()>,
    mut line_tx: mpsc::Sender<String>,
) -> Result<()> {
    loop {
        // BUILDING + RUNNING phase
        let child = match build_and_spawn(&cfg, &mut line_tx).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[wafer dev] ✗ build failed: {e}");
                // Wait for next file change, then retry.
                if change_rx.recv().await.is_none() {
                    return Ok(());
                }
                continue;
            }
        };

        // Wait for either a change event or the child exiting.
        match wait_for_change_or_exit(child, &mut change_rx, cfg.kill_timeout).await {
            ChildOutcome::ChangeRequested => continue,
            ChildOutcome::ExitedZero => {
                // Process exited cleanly on its own (e.g., short-lived app).
                eprintln!("[wafer dev] process exited cleanly. Waiting for changes.");
                if change_rx.recv().await.is_none() {
                    return Ok(());
                }
            }
            ChildOutcome::Crashed(code) => {
                eprintln!("[wafer dev] ✗ process crashed (exit code {code}) — waiting for changes");
                if change_rx.recv().await.is_none() {
                    return Ok(());
                }
            }
            ChildOutcome::WatcherClosed => return Ok(()),
        }
    }
}

enum ChildOutcome {
    ChangeRequested,
    ExitedZero,
    Crashed(i32),
    WatcherClosed,
}

async fn build_and_spawn(
    cfg: &SupervisorConfig,
    line_tx: &mut mpsc::Sender<String>,
) -> Result<Child> {
    // Build first, separate from run, so build failures don't poison runtime
    // detection.
    let mut build = Command::new("cargo");
    build.arg("build");
    if cfg.release {
        build.arg("--release");
    }
    build.arg("--bin").arg(&cfg.bin);
    build.args(&cfg.cargo_args);
    let status = build
        .status()
        .await
        .context("failed to invoke `cargo build`")?;
    if !status.success() {
        anyhow::bail!("cargo build exited with {}", status);
    }

    // Spawn the binary directly to skip cargo's incremental check on the
    // second invocation (cargo run would re-check anyway, costing ~100ms).
    let mut run = Command::new("cargo");
    run.arg("run");
    if cfg.release {
        run.arg("--release");
    }
    run.arg("--bin").arg(&cfg.bin);
    run.arg("--quiet"); // suppress cargo's "Compiling…" lines on the second run
    if !cfg.cargo_args.is_empty() {
        run.arg("--").args(&cfg.cargo_args);
    }
    run.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = run.spawn().context("failed to spawn cargo run")?;

    // Pipe child stdout/stderr to parent terminal AND tee stderr through
    // line_tx for the summary parser.
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                println!("{line}");
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let mut tee = line_tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                eprintln!("{line}");
                let _ = tee.send(line).await;
            }
        });
    }

    Ok(child)
}

async fn wait_for_change_or_exit(
    mut child: Child,
    change_rx: &mut mpsc::Receiver<()>,
    kill_timeout: Duration,
) -> ChildOutcome {
    tokio::select! {
        change = change_rx.recv() => {
            match change {
                Some(()) => {
                    kill_with_grace(&mut child, kill_timeout).await;
                    ChildOutcome::ChangeRequested
                }
                None => {
                    kill_with_grace(&mut child, kill_timeout).await;
                    ChildOutcome::WatcherClosed
                }
            }
        }
        status = child.wait() => {
            match status {
                Ok(s) if s.success() => ChildOutcome::ExitedZero,
                Ok(s) => ChildOutcome::Crashed(s.code().unwrap_or(-1)),
                Err(_) => ChildOutcome::Crashed(-1),
            }
        }
    }
}

#[cfg(unix)]
async fn kill_with_grace(child: &mut Child, timeout: Duration) {
    let pid = match child.id() {
        Some(p) => p as i32,
        None => return,
    };
    // SIGTERM
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    // Wait up to `timeout` for graceful exit, then SIGKILL.
    if tokio::time::timeout(timeout, child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
}

#[cfg(not(unix))]
async fn kill_with_grace(child: &mut Child, _timeout: Duration) {
    // Windows: TerminateProcess is the only option; no graceful equivalent.
    let _ = child.kill().await;
}
```

Note: this introduces a `libc` dep on Unix (or you could use `nix` if it's already in workspace). Add to `crates/wafer-cli/Cargo.toml`:

```toml
[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

- [ ] **Step 2: Wire the supervisor into `mod.rs`**

Open `crates/wafer-cli/src/commands/dev/mod.rs`. Add `mod supervisor;`. Replace the placeholder loop in `run()` with:

```rust
pub async fn run(args: DevArgs) -> Result<()> {
    let manifest = PathBuf::from("Cargo.toml");
    let detected = bin_detect::detect_bins(&manifest)?;
    let bin = bin_detect::resolve_bin(detected, args.bin.as_deref())?;
    let debounce = Duration::from_millis(args.debounce);

    let patterns = watcher::merge_patterns(&args.watch, !args.no_default_watch);
    let change_rx = watcher::spawn_watcher(&patterns, debounce)?;

    let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<String>(64);

    // Drain line_rx for now (Task 6 will consume these for the summary).
    tokio::spawn(async move {
        while line_rx.recv().await.is_some() {}
    });

    let cfg = supervisor::SupervisorConfig {
        bin: bin.clone(),
        release: args.release,
        cargo_args: args.cargo_args.clone(),
        kill_timeout: Duration::from_secs(args.kill_timeout),
    };

    eprintln!("[wafer dev] watching {patterns:?}; running bin = {bin}");
    supervisor::run(cfg, change_rx, line_tx).await
}
```

- [ ] **Step 3: Build sanity check**

```bash
cargo build -p wafer-cli 2>&1 | tail -3
```

Expected: clean compile.

- [ ] **Step 4: Smoke-test against hello-world**

```bash
(cd examples/hello-world && timeout 8s cargo run --bin wafer --manifest-path ../../crates/wafer-cli/Cargo.toml -- dev &)
sleep 6
touch examples/hello-world/src/main.rs
sleep 2
pkill -f 'wafer-cli' 2>/dev/null
```

Expected: cargo run starts hello-world, the binary boots and prints its "listening" log, after the touch the supervisor kills + restarts. No banner yet (Task 6 adds it).

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-cli/Cargo.toml crates/wafer-cli/src/commands/dev/supervisor.rs crates/wafer-cli/src/commands/dev/mod.rs
git commit -m "feat(cli): process supervisor with build+spawn+kill cascade for wafer dev"
```

---

### Task 6: Boot summary parser + banner

**Files:**
- Create: `crates/wafer-cli/src/commands/dev/summary.rs`
- Modify: `crates/wafer-cli/src/commands/dev/mod.rs`

- [ ] **Step 1: Write failing tests for the parser**

Create `crates/wafer-cli/src/commands/dev/summary.rs`:

```rust
//! Boot-event tracing parser.
//!
//! `wafer dev` tees the child's stderr lines through `parse_event`. Lines
//! that contain `target = "wafer.runtime"` events are extracted and used to
//! build the boot banner. Other lines are ignored.

use once_cell::sync::Lazy;
use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootEvent {
    Starting { blocks: usize },
    FlowRegistered { flow: String },
    Listening { addr: String },
}

/// Match the structured fields emitted by tracing's default formatter.
///
/// Example line (single-line subscriber, ANSI stripped):
///   2026-04-30T10:00:00.000Z  INFO wafer-run::runtime::lifecycle: wafer runtime starting
///       blocks=12 event="starting"
///
/// We don't pin the prefix; we look for `event="..."` and the relevant fields
/// anywhere on the line (tracing's field formatter is stable across versions).
static EVENT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"event="([^"]+)""#).unwrap());
static BLOCKS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"\bblocks=(\d+)\b"#).unwrap());
static FLOW_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"\bflow=(\S+)"#).unwrap());
static ADDR_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"\baddr=(\S+)"#).unwrap());

/// Try to extract a BootEvent from a single stderr line. Returns `None` for
/// any line that isn't a wafer.runtime event.
pub fn parse_event(line: &str) -> Option<BootEvent> {
    // Cheap pre-filter: skip lines that don't even mention "wafer.runtime".
    // tracing's default formatter prints the target before the message, e.g.
    //   ... INFO wafer-run::runtime::...: ...
    // We use the `event=` field as the discriminator — every wafer.runtime
    // event has it, and no non-wafer log line should contain `event="…"` from
    // the wafer codebase.
    let event = EVENT_RE.captures(line)?.get(1)?.as_str();
    match event {
        "starting" => {
            let blocks = BLOCKS_RE
                .captures(line)?
                .get(1)?
                .as_str()
                .parse()
                .ok()?;
            Some(BootEvent::Starting { blocks })
        }
        "flow_registered" => {
            let flow = FLOW_RE.captures(line)?.get(1)?.as_str().to_string();
            Some(BootEvent::FlowRegistered { flow })
        }
        "listening" => {
            let addr = ADDR_RE.captures(line)?.get(1)?.as_str().to_string();
            Some(BootEvent::Listening { addr })
        }
        _ => None,
    }
}

/// Pretty-format the boot banner. Missing fields are omitted gracefully.
pub fn format_banner(blocks: Option<usize>, flows: usize, addr: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(addr) = addr {
        parts.push(format!("→ {addr}"));
    }
    let mut counts = Vec::new();
    if let Some(b) = blocks {
        counts.push(format!("{b} blocks"));
    }
    if flows > 0 {
        counts.push(format!("{flows} flows"));
    }
    if !counts.is_empty() {
        parts.push(format!("({})", counts.join(", ")));
    }
    if let Some(addr) = addr {
        let pretty = pretty_url(addr);
        parts.push(format!("· {pretty}"));
    }
    if parts.is_empty() {
        "✓ wafer dev → ready".to_string()
    } else {
        format!("✓ wafer dev {}", parts.join(" "))
    }
}

fn pretty_url(addr: &str) -> String {
    // 0.0.0.0:8080 → http://localhost:8080
    // [::]:8080 → http://localhost:8080
    // 127.0.0.1:3000 → http://localhost:3000
    let port = addr.rsplit(':').next().unwrap_or("");
    if port.parse::<u16>().is_ok() {
        format!("http://localhost:{port}")
    } else {
        format!("http://{addr}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_starting_event() {
        let line = r#"2026-04-30T10:00:00.000Z  INFO wafer-run: wafer runtime starting blocks=12 event="starting""#;
        assert_eq!(parse_event(line), Some(BootEvent::Starting { blocks: 12 }));
    }

    #[test]
    fn parses_flow_registered_event() {
        let line = r#"2026-04-30T10:00:00.000Z  INFO wafer-run: registered flow flow=site-main event="flow_registered""#;
        assert_eq!(
            parse_event(line),
            Some(BootEvent::FlowRegistered { flow: "site-main".into() })
        );
    }

    #[test]
    fn parses_listening_event() {
        let line = r#"2026-04-30T10:00:00.000Z  INFO wafer-run: wafer-run/http-listener listening addr=0.0.0.0:8080 event="listening""#;
        assert_eq!(
            parse_event(line),
            Some(BootEvent::Listening { addr: "0.0.0.0:8080".into() })
        );
    }

    #[test]
    fn ignores_non_wafer_lines() {
        let line = r#"2026-04-30T10:00:00.000Z  INFO some_other_crate: hello world"#;
        assert_eq!(parse_event(line), None);
    }

    #[test]
    fn banner_with_all_fields() {
        let banner = format_banner(Some(12), 3, Some("0.0.0.0:8080"));
        assert!(banner.contains("12 blocks"));
        assert!(banner.contains("3 flows"));
        assert!(banner.contains("http://localhost:8080"));
    }

    #[test]
    fn banner_with_missing_fields() {
        let banner = format_banner(None, 0, None);
        assert_eq!(banner, "✓ wafer dev → ready");
    }

    #[test]
    fn pretty_url_strips_bind_addr() {
        assert_eq!(pretty_url("0.0.0.0:8080"), "http://localhost:8080");
        assert_eq!(pretty_url("127.0.0.1:3000"), "http://localhost:3000");
    }
}
```

Note: `once_cell` is required. Check if `wafer-cli` already depends on it (`grep -E '^once_cell' crates/wafer-cli/Cargo.toml`). If not, add `once_cell = "1"`.

- [ ] **Step 2: Run tests**

```bash
cargo test -p wafer-cli commands::dev::summary:: -- --nocapture
```

Expected: 7 tests pass.

- [ ] **Step 3: Wire the summary into `mod.rs`**

Open `crates/wafer-cli/src/commands/dev/mod.rs`. Add `mod summary;`. Replace the placeholder line-draining loop with the actual consumer:

```rust
pub async fn run(args: DevArgs) -> Result<()> {
    use summary::BootEvent;

    let manifest = PathBuf::from("Cargo.toml");
    let detected = bin_detect::detect_bins(&manifest)?;
    let bin = bin_detect::resolve_bin(detected, args.bin.as_deref())?;
    let debounce = Duration::from_millis(args.debounce);

    let patterns = watcher::merge_patterns(&args.watch, !args.no_default_watch);
    let change_rx = watcher::spawn_watcher(&patterns, debounce)?;

    let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<String>(64);

    // Summary aggregator: accumulates per-spawn state, prints the banner on
    // `event = "listening"`. Reset on each new "starting" event.
    tokio::spawn(async move {
        let mut blocks: Option<usize> = None;
        let mut flows: usize = 0;
        while let Some(line) = line_rx.recv().await {
            match summary::parse_event(&line) {
                Some(BootEvent::Starting { blocks: n }) => {
                    blocks = Some(n);
                    flows = 0;
                }
                Some(BootEvent::FlowRegistered { .. }) => {
                    flows += 1;
                }
                Some(BootEvent::Listening { addr }) => {
                    let banner = summary::format_banner(blocks, flows, Some(&addr));
                    eprintln!("[wafer dev] {banner}");
                }
                None => {}
            }
        }
    });

    let cfg = supervisor::SupervisorConfig {
        bin: bin.clone(),
        release: args.release,
        cargo_args: args.cargo_args.clone(),
        kill_timeout: Duration::from_secs(args.kill_timeout),
    };

    eprintln!("[wafer dev] watching {patterns:?}; running bin = {bin}");
    supervisor::run(cfg, change_rx, line_tx).await
}
```

- [ ] **Step 4: Verify the full build still works**

```bash
cargo build -p wafer-cli 2>&1 | tail -3
cargo test -p wafer-cli --lib 2>&1 | tail -3
```

Expected: clean build, all unit tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-cli/Cargo.toml crates/wafer-cli/src/commands/dev/summary.rs crates/wafer-cli/src/commands/dev/mod.rs
git commit -m "feat(cli): boot summary parser + banner for wafer dev"
```

---

### Task 7: Integration test with fake-app fixture

**Files:**
- Create: `crates/wafer-cli/tests/fixtures/dev-fake-app/Cargo.toml`
- Create: `crates/wafer-cli/tests/fixtures/dev-fake-app/src/main.rs`
- Create: `crates/wafer-cli/tests/dev.rs`

The fixture is a tiny binary that emits the three structured tracing events on startup, then sleeps. The integration test launches it via `cargo run` (mirroring what `wafer dev` does), drives the supervisor, and asserts the banner appears.

- [ ] **Step 1: Create the fake app fixture**

Create `crates/wafer-cli/tests/fixtures/dev-fake-app/Cargo.toml`:

```toml
[package]
name = "dev-fake-app"
version = "0.0.0"
edition = "2021"
publish = false

[workspace]
# Standalone workspace so it doesn't pollute the main wafer-run target dir.

[dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread", "time"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["fmt"] }
```

Create `crates/wafer-cli/tests/fixtures/dev-fake-app/src/main.rs`:

```rust
//! Test fixture for `wafer dev` integration tests. Emits the same structured
//! tracing events the real wafer-run runtime emits at boot, then sleeps.

#[tokio::main]
async fn main() {
    // Match the line shape the parser expects: tracing-subscriber default
    // formatter with structured fields after the message.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr) // stderr, like wafer-run's default
        .with_ansi(false)
        .init();

    tracing::info!(
        target: "wafer.runtime",
        event = "starting",
        blocks = 4,
        "wafer runtime starting"
    );

    for flow in ["flow-a", "flow-b"] {
        tracing::info!(
            target: "wafer.runtime",
            event = "flow_registered",
            flow = %flow,
            "registered flow"
        );
    }

    tracing::info!(
        target: "wafer.runtime",
        event = "listening",
        addr = %"0.0.0.0:9999",
        "wafer-run/http-listener listening"
    );

    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
}
```

- [ ] **Step 2: Write the integration test**

Create `crates/wafer-cli/tests/dev.rs`:

```rust
//! Integration test for `wafer dev`. Runs against a fake-app fixture that
//! emits the three tracing events the real runtime emits.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

const FIXTURE_DIR: &str = "tests/fixtures/dev-fake-app";

#[tokio::test(flavor = "multi_thread")]
async fn dev_emits_banner_against_fake_app() {
    // Pre-build the fixture so the dev loop sees a fast startup.
    let pre = Command::new("cargo")
        .arg("build")
        .current_dir(FIXTURE_DIR)
        .status()
        .await
        .expect("pre-build fixture");
    assert!(pre.success(), "fixture pre-build failed");

    // Run wafer-cli dev pointing into the fixture.
    // Note: wafer-cli's dev reads Cargo.toml from cwd, so we cd in.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_wafer"));
    cmd.arg("dev")
        .arg("--debounce")
        .arg("100")
        .arg("--kill-timeout")
        .arg("1")
        .current_dir(FIXTURE_DIR)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("spawn wafer dev");

    // Read stderr until we see the banner or timeout.
    let stderr = child.stderr.take().expect("stderr");
    let mut reader = BufReader::new(stderr).lines();

    let banner_seen = timeout(Duration::from_secs(60), async {
        while let Ok(Some(line)) = reader.next_line().await {
            if line.contains("[wafer dev] ✓ wafer dev") && line.contains("9999") {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    // Tear down the dev loop.
    let _ = child.kill().await;
    let _ = child.wait().await;

    assert!(banner_seen, "expected boot banner with port 9999 in wafer dev stderr");
}
```

- [ ] **Step 3: Run the integration test**

```bash
cargo test -p wafer-cli --test dev -- --nocapture
```

Expected: passes within ~30 seconds (build of fixture + cargo build inside dev + run + parse).

If it fails on a timing edge (e.g., 60s isn't enough on a cold machine), bump the timeout in the test to 120s. If it fails on banner-not-found, run with `--nocapture` and inspect stderr to see what lines came through.

- [ ] **Step 4: Commit**

```bash
git add crates/wafer-cli/tests/dev.rs crates/wafer-cli/tests/fixtures/
git commit -m "test(cli): integration test for wafer dev against fake-app fixture"
```

---

### Task 8: Documentation

**Files:**
- Modify: `/home/joris/Programs/suppers-ai/workspace/site/content/docs/cli.html`

The wafer.run/docs CLI reference must list the new subcommand. (`crates/wafer-cli/src/main.rs` already gets a doc comment from Task 3 Step 6.)

- [ ] **Step 1: Read the existing CLI page to match its shape**

```bash
head -80 /home/joris/Programs/suppers-ai/workspace/site/content/docs/cli.html
grep -nE '<h2|<h3|wafer (search|info|install|publish|new)' /home/joris/Programs/suppers-ai/workspace/site/content/docs/cli.html | head -30
```

The page lists each subcommand under an `<h3>` heading with: a one-paragraph description, a usage block (`<pre>`), and a flag list. Match this format for `wafer dev`.

- [ ] **Step 2: Add the `wafer dev` section**

Insert after the existing last command section (likely `wafer install`) but before the page footer:

```html
<h3 id="dev">wafer dev</h3>

<p>Watches a wafer-run app's source files and restarts the running process on every save. Prints a wafer-aware boot summary on each successful start (block count, flow count, listening address). Use this as your local inner loop instead of manually <code>cargo run</code>-ing after every edit.</p>

<pre><code>wafer dev [--bin NAME] [--release] [--watch PATTERN]... [-- &lt;cargo args&gt;]</code></pre>

<dl>
  <dt><code>--bin &lt;NAME&gt;</code></dt>
  <dd>Cargo bin target to run. Auto-detected from <code>Cargo.toml</code> if there's exactly one bin.</dd>

  <dt><code>--release</code></dt>
  <dd>Pass <code>--release</code> to cargo. Default is dev profile (faster rebuilds).</dd>

  <dt><code>--watch &lt;PATTERN&gt;</code></dt>
  <dd>Add an extra path or glob to watch. Repeatable. Default watch list: <code>src/**/*.rs</code>, <code>Cargo.toml</code>, <code>wafer.lock</code>.</dd>

  <dt><code>--no-default-watch</code></dt>
  <dd>Disable the default watch list (use only <code>--watch</code> patterns).</dd>

  <dt><code>--debounce &lt;MS&gt;</code></dt>
  <dd>File-change debounce window in milliseconds. Default 200.</dd>

  <dt><code>--kill-timeout &lt;SEC&gt;</code></dt>
  <dd>SIGTERM → SIGKILL grace period. Default 3.</dd>
</dl>

<p>The boot summary requires the runtime's structured <code>wafer.runtime</code> tracing events (added in Spec 3B). Apps using older runtime versions still work — <code>wafer dev</code> falls back to a "no listener event yet" banner.</p>
```

- [ ] **Step 3: Add `dev` to any sidebar / table-of-contents on the page**

If `cli.html` has a `<nav>` or `<ul>` listing each command at the top, add an entry for `wafer dev` matching the existing pattern. (Check the file's structure; if it has no such index, skip this step.)

- [ ] **Step 4: Verify the site renders the new page locally**

```bash
cd /home/joris/Programs/suppers-ai/workspace/site
pkill -f "cargo run" 2>/dev/null || true
sleep 0.5
set -a && . ./.env && set +a
cargo run --release > /tmp/wafer-site.log 2>&1 &
SERVER_PID=$!
until grep -q 'wafer-site listening' /tmp/wafer-site.log 2>/dev/null; do
  sleep 1
  if ! kill -0 $SERVER_PID 2>/dev/null; then
    echo "BUILD FAILED — see /tmp/wafer-site.log"; exit 1
  fi
done

curl -s http://localhost:8090/docs/cli | grep -c 'wafer dev'
# Expected: 3+ (anchor + heading + pre + body mentions)

kill $SERVER_PID 2>/dev/null || true
```

- [ ] **Step 5: Commit (in the site repo, NOT wafer-run)**

The site is a separate git repo. The doc change lands there:

```bash
cd /home/joris/Programs/suppers-ai/workspace/site
git status -s                              # only content/docs/cli.html should be modified
git checkout -b feat/cli-dev-docs
git add content/docs/cli.html
git commit -m "docs: document wafer dev command"
git push -u origin feat/cli-dev-docs       # ⚠ requires user go-ahead per workspace PR rule
```

⚠ This step pushes to the site repo. Pause here and ask the user before pushing. Open a separate PR on the site repo.

Coordination note: the wafer-run PR (next task) references `wafer.run/docs/cli` which won't include `wafer dev` until the site PR merges. That's acceptable: the site link is "soon-to-be" content; the PR description in Task 9 calls this out.

- [ ] **Step 6: Return to the wafer-run repo to continue Task 9**

```bash
cd /home/joris/Programs/suppers-ai/workspace/wafer-run
```

---

### Task 9: Verification + push + PR (gated)

**Files:** none (verification only)

This task pushes shared state. Pause for explicit user confirmation before any remote action.

- [ ] **Step 1: Full test suite (CI-mirror)**

```bash
cargo +nightly fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace --exclude wafer-run && cargo test -p wafer-run --lib
cargo test -p wafer-cli --test dev
```

Expected: all clean.

- [ ] **Step 2: Show user the local state**

```bash
git log --oneline main..feat/wafer-dev
git diff --stat main..feat/wafer-dev
```

Expected: ~7 commits, ~12-15 files changed (3 runtime files, 1 Cargo.toml, 6 new dev/ files, main.rs/commands/mod.rs, fixture, integration test, spec, plan).

Wait for user "go ahead".

- [ ] **Step 3: Push the branch**

```bash
git push -u origin feat/wafer-dev 2>&1 | tail -5
```

Expected: `branch 'feat/wafer-dev' set up to track 'origin/feat/wafer-dev'.`

- [ ] **Step 4: Open the PR**

```bash
gh pr create --title "feat(cli): wafer dev — file-watch loop with wafer-aware boot summary (Spec 3B)" --body "$(cat <<'EOF'
## Summary

Adds the \`wafer dev\` subcommand: file-watching dev loop that kills+respawns a \`cargo run\` child on save, with a wafer-aware boot summary on each successful start.

Closes the inner-loop iteration gap identified in [Spec 3B](./docs/specs/2026-04-30-wafer-dev-command-design.md). Today every change to a wafer-run app requires manual ctrl-C → \`cargo run\` → wait. After this PR, the loop is automatic and the post-restart state is visible at a glance.

### What changed

- **\`wafer dev\` subcommand** in \`wafer-cli\`. Watches \`src/**/*.rs\`, \`Cargo.toml\`, \`wafer.lock\` by default; adds extras via \`--watch <pattern>\`. SIGTERM → SIGKILL kill cascade with a \`--kill-timeout\` grace. Auto-detects the bin target; supports \`--bin NAME\` for multi-bin workspaces.
- **Boot summary banner.** Single line on each successful start: \`✓ wafer dev → 0.0.0.0:8080 (12 blocks, 3 flows) · http://localhost:8080\`. Sourced from three new structured tracing events.
- **Three structured \`wafer.runtime\` tracing events** added to the runtime: \`event = "starting"\` (in \`Wafer::start\`), \`event = "flow_registered"\` (in \`Wafer::add_flow_json\`), \`event = "listening"\` (in \`wafer-block-http-listener\`). These are documented as a stable contract for tooling.
- **Integration test** with a fake-app fixture that emits the three events and sleeps; \`wafer dev\` is driven against it and the banner is asserted in stderr.

### What's NOT in this PR

- No in-process flow reload — every restart is a cold process restart.
- No external flow-file convention. Flow JSON stays inline in Rust source.
- No \`wafer new-flow\` scaffolder. Deferred to 3B.5 if a real ask surfaces.
- No log filtering, coloring, browser auto-open. Pass-through stdout/stderr verbatim.

### Spec & plan

- Design: \`docs/specs/2026-04-30-wafer-dev-command-design.md\`
- Plan: \`docs/plans/2026-04-30-wafer-dev-command.md\`

### Test plan

- [x] \`cargo +nightly fmt --all -- --check\` passes.
- [x] \`cargo clippy --workspace -- -D warnings\` clean.
- [x] \`cargo test --workspace --exclude wafer-run && cargo test -p wafer-run --lib\` passes.
- [x] \`cargo test -p wafer-cli --test dev\` passes (integration test against fake-app fixture).
- [x] Manual smoke: \`wafer dev\` in \`examples/hello-world/\`, edit \`src/main.rs\`, observe banner reappear.

### Initiative context

This is sub-spec B of Spec 3 (Developer Experience). Spec 3A docs (PR #28) lands first; Spec 3C (JS SDK host runtime) is queued behind this. The runtime tracing events introduced here are intentionally minimal; if 3B.5 (log polish) lands, it consumes the same contract.

### Companion PR

Documentation for \`wafer dev\` lives at \`wafer.run/docs/cli\` — a separate PR against \`github.com/wafer-run/site\`. Until that merges, the README's link to \`wafer.run/docs/cli\` will not show \`wafer dev\` content; the wafer-run PR doesn't depend on the site PR.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 5: Verify**

```bash
gh pr view --json url --jq .url
```

---

## Self-review

**Spec coverage:**

| Spec section / requirement                                       | Implemented in                  |
| ---------------------------------------------------------------- | ------------------------------- |
| Architecture: code in `crates/wafer-cli/src/commands/dev/`       | Tasks 3-6                       |
| Architecture: deps `notify`, `notify-debouncer-mini`, `regex`    | Task 3 Step 1                   |
| Concurrency model: watcher → mpsc → supervisor                   | Tasks 4 (watcher), 5 (supervisor) |
| CLI surface: all 7 flags                                         | Task 3 Step 5 (DevArgs struct)  |
| Bin auto-detection                                               | Task 3 Step 3 + Task 3 Step 5   |
| Runtime touch: 3 structured events with `target = "wafer.runtime"` | Task 2                       |
| Boot summary: parse tracing → format banner                      | Task 6                          |
| Banner format with all/some/no fields                            | Task 6 Step 1 (tests)           |
| File watch: defaults + user patterns + debounce                  | Task 4                          |
| Process lifecycle: state machine, kill cascade, no auto-restart on crash | Task 5                  |
| Output handling: stdout passthrough, stderr passthrough + tee   | Task 5 Step 1 (build_and_spawn) |
| Testing: unit (3 modules), integration (fake-app), manual smoke  | Tasks 3, 4, 5, 6 (unit), 7 (integration), 9 (manual via PR test plan) |
| Documentation                                                    | Task 8                          |
| Branch + PR (workspace rule)                                     | Tasks 1, 9                      |

**Placeholder scan:** No `TBD` / `TODO` / "implement later". Each step has full code or exact commands. Two intentional `_` underscored variables in Task 4 Step 1 are leftover scaffolding flagged in the step's note ("delete that line; the function returns `Ok(async_rx)` directly") — that's an instruction, not a placeholder.

**Type / identifier consistency:**
- Module names consistent: `bin_detect`, `watcher`, `supervisor`, `summary` — all under `commands::dev`.
- Function names consistent: `detect_bins` / `detect_bins_from_str` / `resolve_bin` (Task 3); `merge_patterns` / `spawn_watcher` (Task 4); `build_and_spawn` / `wait_for_change_or_exit` / `kill_with_grace` (Task 5); `parse_event` / `format_banner` / `pretty_url` (Task 6).
- Types consistent: `DetectedBin`, `DevArgs`, `SupervisorConfig`, `BootEvent`, `Restart`, `ChildOutcome`. `Restart` defined in Task 5 but currently unused (the channel signals via `()`); kept in case the supervisor grows reasons to distinguish event causes — if not, drop in self-review during implementation.
- Tracing event names consistent: `starting`, `flow_registered`, `listening` across runtime emit (Task 2), parser (Task 6 Step 1), fixture (Task 7 Step 1).
- Branch name `feat/wafer-dev` consistent across Task 1 and Task 9.
- File paths consistent.

**Out-of-scope creep:** None. No flow-file convention, no scaffolder, no log polish, no in-process reload — all matches the spec's non-goals.
