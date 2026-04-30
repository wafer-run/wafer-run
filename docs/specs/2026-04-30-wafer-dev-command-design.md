# wafer-run Spec 3B — `wafer dev` (CLI hot-reload)

**Status:** Draft (2026-04-30)
**Initiative:** Hardening Spec 3 (Developer Experience), sub-spec B (CLI DX).
**Predecessors:** Spec 1 (PR #3), Spec 2A (PR #4), Spec 2B (PR #5), Spec 3A (PR #28, in-flight).
**Successor:** Spec 3C — JS SDK host runtime.

## Goal

Add a `wafer dev` subcommand to `wafer-cli` that wraps `cargo run` with file watching, debounced rebuilds, kill-and-respawn supervision, and a wafer-aware boot summary. Closes the local inner-loop gap: today, every change to a wafer-run app requires manual ctrl-C → `cargo run` → wait. After this spec, the loop is automatic and the post-restart state is visible at a glance.

## Non-goals

- **No in-process flow reload.** Every restart is a cold process restart; the runtime is not modified to swap flow tables in-place. (Brainstorm Q1 = B, not C.)
- **No external flow-file convention.** Flow JSON stays in Rust source (as `&'static str` constants, e.g., solobase's `site_main::JSON`, or inline `serde_json::json!{}` literals). No new loader API on `wafer-flow-http-server`. (Q2 = A.)
- **No `wafer new-flow` / app scaffolder.** The hardening-state memory listed both `wafer dev` and `wafer new-flow` in 3B; the scaffolder is dropped. No current consumer is asking for it; defer to 3B.5 if a real ask surfaces. (Q3 = A.)
- **No log filtering / coloring / browser auto-open.** Pass-through child stdout/stderr verbatim. Polish items deferred. (Q4 = A, not C.)
- **No per-block / per-crate targeted rebuilds.** Cargo's incremental compilation already handles speed; layering wafer-aware change-detection on top would duplicate work cargo already does correctly.
- **No multi-binary supervision.** A `wafer dev` invocation manages one child process. Multi-bin workspaces use `--bin <name>`.

## Audience

A developer iterating on a wafer-run application (a `[bin]` crate that depends on `wafer-run`). Two concrete shapes:

- **Hand-crafted apps** like `examples/hello-world/`, `examples/api-server/`, etc. — `Cargo.toml` + `src/main.rs` with inline flow JSON.
- **Production-shaped apps** like solobase — `Cargo.toml` + multi-crate workspace, flow JSON in `pub const X: &str = r#"..."#;` constants, runtime configuration via `Wafer::add_block_config()`.

Both shapes route every change through `cargo build`. `wafer dev` accepts that and adds value above it (rebuild-on-save + boot summary + clean error surfacing), rather than trying to bypass cargo for a faster path that wouldn't work for solobase anyway.

## Architecture

`wafer dev` is a new subcommand in `wafer-cli`. All new code lives in:

- `crates/wafer-cli/src/commands/dev.rs` — the entrypoint and CLI clap config for the subcommand.
- `crates/wafer-cli/src/commands/dev/` — submodule directory if `dev.rs` grows beyond ~200 LOC. Likely splits: `watcher.rs` (notify wiring), `supervisor.rs` (process state machine), `summary.rs` (boot-event parser).

New `wafer-cli` deps:

- `notify` (file system watching, latest stable on crates.io).
- `notify-debouncer-mini` (debounce + glob support layered on `notify`).

Already-available deps: `tokio` (process supervision via `tokio::process::Child`), `anyhow` (error context), `tracing` (CLI's own logs), `clap` (subcommand definition).

No changes to runtime crates **except** a small structured-tracing addition (see § Runtime touch below). No changes to any block crate. No changes to `wafer-flow-http-server`.

### Concurrency model

A single `tokio::main` task spawns three concurrent sub-tasks via `tokio::select!`:

1. **Watcher.** `notify-debouncer-mini` events flow through a 200ms debounce window into a `tokio::sync::mpsc::Sender<Restart>`.
2. **Supervisor.** Owns one `tokio::process::Child` (the running `cargo run` instance). Receives `Restart` events from the channel; on each event, runs `kill_with_grace` then `build_and_spawn`.
3. **Output streamer.** Two child-spawned tasks pipe child stdout/stderr to the parent terminal unchanged. The stderr stream additionally tees a copy through `summary::parse_boot_event` so the supervisor can emit the boot banner when `event = "listening"` arrives.

A single `Ctrl-C` from the user shuts everything down: the supervisor sends SIGTERM to the child, waits up to `--kill-timeout` seconds, SIGKILLs if necessary, then returns from `tokio::main`.

## CLI surface

```
wafer dev [OPTIONS] [-- <cargo args>...]

Options:
  --bin <NAME>          Cargo bin target to run. Default: auto-detected from Cargo.toml;
                        if multiple bins exist, error and ask the user to pass --bin.
  --release             Pass --release to cargo. Default: dev profile (faster rebuilds).
  --watch <PATTERN>     Add an extra path/glob to watch (repeatable). Defaults below.
  --no-default-watch    Disable the default watch list (use only --watch patterns).
  --debounce <MS>       File-change debounce window. Default: 200.
  --kill-timeout <SEC>  SIGTERM → SIGKILL grace period. Default: 3.

Default watch list:
  - src/**/*.rs
  - Cargo.toml
  - wafer.lock        (if present)
```

Anything after `--` is forwarded verbatim to `cargo run`, e.g. `wafer dev -- --features foo`.

Bin auto-detection:

1. Read `Cargo.toml` from cwd.
2. Enumerate `[[bin]]` entries plus `src/main.rs` (which is `[[bin]]` of name = package name by cargo's auto-detection).
3. If exactly 1, use it. If 0, error: "no bin target found — wafer dev needs a [[bin]] crate". If >1, error: "multiple bin targets found ({list}); pass --bin <name>".

## Runtime touch — structured tracing events

`wafer dev`'s boot summary is the differentiator and depends on a stable signal that the runtime has finished starting. Two candidate sources:

- **Mechanism A (chosen) — parse `tracing` events.** Robust to any consumer (HTTP-less apps, custom ports, multi-listener setups), no out-of-band probes.
- **Mechanism B (rejected) — HTTP probe** to `/_inspector/blocks` after spawn. Brittle: requires inspector block, requires knowing the port (not exposed), breaks for non-HTTP apps.

The runtime today emits error-level events on failure but no structured info-level boot timeline. Add three events with the stable target `"wafer.runtime"`:

| Where | Event | Fields | Emitted |
| --- | --- | --- | --- |
| `wafer-run::runtime::lifecycle::start` | `event = "starting"` | `blocks: usize` | Once, at the top of `Wafer::start`. |
| `wafer-run::runtime::add_flow_json` | `event = "flow_registered"` | `flow: &str` | Per flow as each is registered. Emitted from `Wafer::add_flow_json`, not `start`, because flows are registered pre-start. The supervisor accumulates these in spawn-scoped state and includes the count in the banner once `event = "listening"` arrives. |
| `wafer-block-http-listener` | `event = "listening"` | `addr: &str` | When the listener binds and accepts. (Today: `tracing::info!("wafer-run/http-listener listening on {}", listen)` — restructure to use the stable target + structured fields.) |

These events are a public contract that `wafer dev` parses. Format must be the structured form so we can rely on field-by-field extraction; the textual message can change freely. Example:

```rust
tracing::info!(target: "wafer.runtime", event = "listening", addr = %listen_addr, "wafer listening");
```

The boot banner is printed on `event = "listening"`; if no `listening` event arrives within 30s of spawn, print `⏳ wafer dev → starting (no listener event yet — process running)` so the user isn't left guessing during a slow first build.

For apps without an HTTP listener (rare; would be e.g. a CLI tool built on wafer), the banner stays in the "starting" state — that's fine, the user knows their app has no listener. We don't try to detect "non-HTTP app, banner is N/A."

### Boot banner format

Single-line, after `event = "listening"`:

```
✓ wafer dev → 0.0.0.0:8080 (12 blocks, 3 flows) · http://localhost:8080
```

Field sources:
- Address: from the `addr` field on `event = "listening"`.
- Block count: from the `blocks` field on `event = "starting"` (held in supervisor state since the start event arrives first).
- Flow count: count of `event = "flow_registered"` events received since the last spawn.
- Pretty URL: derived from `addr` — `0.0.0.0:8080` → `http://localhost:8080`, IPv6 brackets handled.

If any field is missing (e.g., a consumer registered no flows): print what we have, omit empty parts. Never error.

## File watch + debounce

`notify-debouncer-mini` with a 200ms window (configurable via `--debounce`).

Default watch list:
- `src/**/*.rs` — Rust source.
- `Cargo.toml` — dependency edits.
- `wafer.lock` — block-version drift (when present; absent for non-CLI apps that don't use `wafer install`).

Behavior:
- **Compile in progress + new change.** Supervisor SIGKILLs the in-flight `cargo build` immediately and starts a fresh build with the new file state. Aborting (rather than waiting) keeps the loop responsive — finishing an already-stale build wastes 5-30s. Cargo's incremental cache makes the restart cheap. Multiple builds never queue: only the most recent change matters.
- **Burst of changes.** Debouncer collapses them into one event. Result: one rebuild per burst.
- **Files outside cwd.** `wafer dev` watches relative to cwd. Workspace members importing siblings need `--watch ../sibling/src/**/*.rs`. We don't auto-discover workspace members in v1 (would be magic).
- **Watcher errors.** `notify` events of `Error` kind log a warning and continue. We do not crash the dev loop on a transient watcher hiccup.

## Process lifecycle

State machine:

```
INIT
  ↓ initial spawn
BUILDING ───→ BUILD_FAILED ──┐
  ↓ build ok                 │
RUNNING                      │
  ↓ file change              │
KILLING                      │
  ↓ child exited             │
BUILDING ←───────────────────┘ (next file change)

If RUNNING child exits non-zero unexpectedly:
RUNNING → CRASHED → (waits for next file change) → BUILDING
```

- **BUILDING:** Run `cargo build` (not `run`). On success, spawn the produced binary directly (`target/<profile>/<bin>`) — skips cargo's incremental check on the second invocation. On failure, transition to `BUILD_FAILED`.
- **BUILD_FAILED:** Banner: `✗ build failed`. The cargo build's stderr is already streamed below the banner via the output streamer. The supervisor parks until the next file change.
- **KILLING:** Send SIGTERM, wait `--kill-timeout` seconds (default 3), SIGKILL if still alive. On Windows, just `Child::kill()` (TerminateProcess).
- **CRASHED:** Banner: `✗ process crashed (exit code N) — waiting for changes`. No auto-restart. Reasoning: a crash usually indicates a bug; auto-restart loops obscure it. The dev edits, saves, watcher fires, normal restart.

## Output handling

- **Child stdout:** piped through to parent stdout unchanged. Line-buffered.
- **Child stderr:** piped through to parent stderr AND tee'd to `summary::parse_boot_event` for structured-event extraction. Lines that don't match a `target=wafer.runtime` event pass through with no transformation.
- **`wafer dev`'s own output** (banners, warnings): printed via the CLI's existing `tracing` setup, prefixed `[wafer dev]` so it's distinguishable from app output.

## Testing strategy

Three layers:

### Unit tests — `crates/wafer-cli/src/commands/dev/`

- `summary::parse_boot_event` — fed sample lines (real lines captured from `wafer-run` runs), asserts the parser extracts `addr`, `blocks`, `flow`, ignores non-runtime lines.
- `watcher::merge_default_and_user_patterns` — defaults + user globs combine without dupes; `--no-default-watch` excludes defaults.
- `cli::detect_bin_target` — parses sample `Cargo.toml` content and returns the right bin name; errors with the right message for 0 / >1 bins.

### Integration tests — `crates/wafer-cli/tests/dev.rs`

- A small fixture binary (`tests/fixtures/dev-fake-app/src/main.rs`) emits the three runtime events on a 100ms timer then sleeps. The integration test:
  1. Launches the fake app via the same `supervisor::build_and_spawn` path used in production.
  2. Asserts the boot banner appears in stdout within 5s.
  3. Touches `tests/fixtures/dev-fake-app/src/main.rs` and asserts a second banner appears within 10s (build + restart cycle).
  4. SIGTERMs the wafer-dev test, asserts it exits within 5s (kill cascade works).
- The fake app does NOT use `wafer-run` — it just prints the right `tracing` lines. This keeps the test independent of real `wafer-run` build times.

### Manual smoke

In the implementation plan, document a manual verification step: run `wafer dev` in `examples/hello-world/`, confirm banner appears at start, edit `src/main.rs` (e.g., change the response string), confirm rebuild + new banner. This is not a CI test (real cargo build of `wafer-run` is too slow for CI).

## Implementation cadence

Single PR. Dependencies on the runtime tracing events are scoped tight enough to land in the same PR without splitting:

- Commit 1: Runtime tracing events (`wafer.runtime.starting/flow_registered/listening`) — small, isolated change to `wafer-run::lifecycle` and `wafer-block-http-listener`. Validates the contract before `wafer dev` consumes it.
- Commit 2: `wafer-cli/dev.rs` skeleton + clap integration + bin auto-detection + unit tests for those.
- Commit 3: Watcher + debouncer wiring + unit tests.
- Commit 4: Supervisor state machine + kill cascade + unit tests.
- Commit 5: Boot summary parser + banner formatting + unit tests.
- Commit 6: Integration test (fake-app fixture).
- Commit 7: Documentation — update `wafer-run/site` `content/docs/cli.html` with `wafer dev`. Update `crates/wafer-cli/src/main.rs` `--help` description.

Branch: `feat/wafer-dev` from `main` (after Spec 3A merges, to avoid conflicting with the new CLI README/CONTRIBUTING).

## Open questions

None. All scope decisions made:
- Q1 (ambition level): B — wafer-aware process reload, no in-process flow swap.
- Q2 (flow-file convention): A — keep flows inline in Rust source.
- Q3 (scaffolder): A — drop `wafer new-flow` from 3B.
- Q4 (feature set): A — tight MVP with boot summary, no log filters / browser auto-open.
- Boot signal mechanism: A — parse tracing events, accept the small runtime touch needed to make them structured and stable.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Tracing line format changes break the parser. | The `target = "wafer.runtime"` + `event = "..."` shape is stated as a runtime contract in this spec. Future changes require coordinated update across runtime + wafer-cli. Capture in `crates/wafer-run/src/runtime/lifecycle.rs` doc comment. |
| `notify` watcher misses changes on certain platforms (e.g., older kernel inotify quirks). | We rely on the existing `notify` ecosystem; documented edge cases (NFS, Docker bind mounts) are out of scope for v1. Document in `wafer dev --help` that it's intended for local-disk projects. |
| Slow first build (tens of seconds for solobase) leaves the user in `INIT/BUILDING` with no banner. | The 30s "no listener event yet" fallback banner reassures the user. Cargo build output streams below it, so progress is visible. |
| Cargo concurrency: developer running `cargo build` separately races `wafer dev`. | Both invocations share `target/`. Cargo's lockfile coordinates. Worst case: `wafer dev`'s build briefly waits for the lock, then proceeds. No data corruption. Document in CONTRIBUTING as a known interaction. |

## Future work (intentionally deferred)

- **Spec 3B.5 — log-output polish.** Filter noisy `tokio` traces, color wafer-relevant lines, browser auto-open on first ready. Ship if developers ask.
- **Spec 4 (or 3C) — external flow-file convention.** New `flow.toml` / `flow.json` next to `Cargo.toml`, loader on `wafer-flow-http-server`, examples migrated. Earns its keep only if multiple consumers ask to edit flows without recompiling.
- **`wafer dev --multi`** — manage multiple bin children for multi-listener apps. No real demand today.
- **`wafer new --kind=app` scaffolder.** When the canonical app shape stabilizes (probably after the flow-file decision), produce a scaffolder. Currently every consumer hand-crafts; not enough convergence yet.
