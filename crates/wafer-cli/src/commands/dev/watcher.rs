//! File-system watching with debounce. Bridges notify's sync mpsc to a tokio
//! mpsc so the supervisor can `await` change events.

use std::{path::Path, sync::mpsc as std_mpsc, time::Duration};

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
///
/// The receiver yields `()` per debounced batch — only the signal that
/// "something changed" matters; the supervisor doesn't care which file.
pub fn spawn_watcher(patterns: &[String], debounce: Duration) -> Result<mpsc::Receiver<()>> {
    let (sync_tx, sync_rx) = std_mpsc::channel::<notify::Result<Vec<DebouncedEvent>>>();
    let (async_tx, async_rx) = mpsc::channel::<()>(8);

    let mut debouncer = new_debouncer(debounce, move |res| {
        sync_tx.send(res).expect("watcher rx dropped")
    })
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
    // The debouncer is moved into the thread so it lives as long as the
    // bridge — dropping it would stop notify from emitting events.
    std::thread::spawn(move || {
        let _debouncer = debouncer;
        while let Ok(res) = sync_rx.recv() {
            match res {
                Ok(events) if !events.is_empty() => {
                    if async_tx.blocking_send(()).is_err() {
                        // Receiver dropped — exit the bridge.
                        break;
                    }
                }
                Ok(_) => {} // empty batch, ignore
                Err(e) => {
                    tracing::warn!(error = ?e, "watcher error (continuing)");
                }
            }
        }
    });

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
