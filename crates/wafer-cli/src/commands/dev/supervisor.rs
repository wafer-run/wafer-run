//! Process state machine for `wafer dev`. Owns one cargo-run child at a time.

use std::{process::Stdio, time::Duration};

use anyhow::{Context, Result};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::mpsc,
};

/// Input to `run()`.
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
    line_tx: mpsc::Sender<String>,
) -> Result<()> {
    loop {
        // BUILDING + RUNNING phase
        let child = match build_and_spawn(&cfg, line_tx.clone()).await {
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

async fn build_and_spawn(cfg: &SupervisorConfig, line_tx: mpsc::Sender<String>) -> Result<Child> {
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
        anyhow::bail!("cargo build exited with {status}");
    }

    // Spawn the binary via cargo run (cargo's incremental check is cheap on
    // the second pass since we just built).
    let mut run = Command::new("cargo");
    run.arg("run");
    if cfg.release {
        run.arg("--release");
    }
    run.arg("--bin").arg(&cfg.bin);
    run.arg("--quiet"); // suppress cargo's "Compiling…" lines
    if !cfg.cargo_args.is_empty() {
        run.arg("--").args(&cfg.cargo_args);
    }
    run.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = run.spawn().context("failed to spawn cargo run")?;

    // Pipe child stdout to parent stdout.
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                println!("{line}");
            }
        });
    }
    // Pipe stderr to parent stderr AND tee through line_tx for the summary.
    if let Some(stderr) = child.stderr.take() {
        let tee = line_tx;
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
    // SAFETY: We hold `&mut Child` across both `child.id()` and the
    // `libc::kill` call. tokio::process::Child reserves the pid until
    // `wait()` is called (zombies are not reaped automatically), and we
    // do not wait between the two calls. Therefore the pid cannot be
    // recycled by the kernel under us, and SIGTERM is delivered to the
    // intended process. Calling libc::kill itself is safe FFI given a
    // valid signal number.
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
