//! `WaferBuilder` — opt-in/out construction over `Wafer::empty()`.
//!
//! Wires Path A (link-time `linkme` of `register_static_block!`-registered
//! blocks) and Path B (`wafer.lock` cache loader) into a single fallible
//! entry point. `Wafer::new()` is `Self::builder().build()`.
//!
//! Resolution order for the lockfile path:
//! 1. `WaferBuilder::lockfile(path)` — explicit override.
//! 2. `WAFER_LOCKFILE` env var.
//! 3. `./wafer.lock` in CWD.
//!
//! Missing at the default location is silent (zero-config friction).
//! Missing at an explicit path errors loudly (misconfig surfaces).
//!
//! On `wasm32` targets there is no filesystem, env, or cwd, so the `Auto`
//! branch is compiled out — symmetric with `load_inventory_blocks()` (Path A),
//! which is also gated. Explicit `.lockfile(path)` still compiles on every
//! target and errors loudly when the path can't be read.

use std::{path::PathBuf, sync::Arc};

use wafer_block::error::RuntimeError;

use crate::runtime::{
    config_source::{ConfigSource, StaticConfigSource},
    Wafer,
};

/// How `WaferBuilder` should locate `wafer.lock`.
#[derive(Debug, Clone)]
pub(crate) enum LockfileSource {
    /// Resolve via `WAFER_LOCKFILE` env var, then fall back to `./wafer.lock` in CWD.
    ///
    /// Behaviour:
    /// - `WAFER_LOCKFILE` set and non-empty: the named path is **required**;
    ///   missing → error. (User opted in by setting the env var.)
    /// - `WAFER_LOCKFILE` unset/empty: `./wafer.lock` is **optional**; missing → no-op.
    Auto,
    /// User-supplied path. Errors if missing.
    Explicit(PathBuf),
    /// Skip lockfile loading entirely.
    Disabled,
}

/// Builder for a `Wafer` runtime with auto-registration knobs.
///
/// Both Path A (linkme static registration) and Path B (lockfile) default
/// to enabled. Use `disable_inventory()` / `disable_lockfile()` to opt out.
#[derive(Clone)]
pub struct WaferBuilder {
    enable_inventory: bool,
    lockfile: LockfileSource,
    config_source: Arc<dyn ConfigSource>,
}

impl Default for WaferBuilder {
    fn default() -> Self {
        Self {
            enable_inventory: true,
            lockfile: LockfileSource::Auto,
            config_source: Arc::new(StaticConfigSource::default()),
        }
    }
}

impl WaferBuilder {
    /// Skip the link-time `linkme` pass. Useful for tests that want
    /// a pristine runtime without picking up `register_static_block!`-annotated
    /// blocks that happen to be linked into the test binary.
    pub fn disable_inventory(mut self) -> Self {
        self.enable_inventory = false;
        self
    }

    /// Skip lockfile loading entirely.
    pub fn disable_lockfile(mut self) -> Self {
        self.lockfile = LockfileSource::Disabled;
        self
    }

    /// Use an explicit lockfile path. Errors at `build()` time if the
    /// file is missing — opposite of `Auto`'s silent no-op.
    pub fn lockfile(mut self, path: impl Into<PathBuf>) -> Self {
        self.lockfile = LockfileSource::Explicit(path.into());
        self
    }

    /// Supply the [`ConfigSource`] the runtime consults on first init for each
    /// block. Defaults to an empty [`StaticConfigSource`]; production callers
    /// install env/D1 sources here.
    pub fn config_source(mut self, source: Arc<dyn ConfigSource>) -> Self {
        self.config_source = source;
        self
    }

    /// Construct the `Wafer`. Runs Path A first, then Path B.
    pub fn build(self) -> Result<Wafer, RuntimeError> {
        let mut w = Wafer::empty();
        w.config = crate::runtime::config_source::ConfigState::new(self.config_source);
        #[cfg(not(target_arch = "wasm32"))]
        if self.enable_inventory {
            w.load_inventory_blocks()?;
        }
        match self.lockfile {
            LockfileSource::Auto => {
                // Auto-discovery requires a filesystem (env var lookup of
                // WAFER_LOCKFILE, fallback to ./wafer.lock). On wasm32 there
                // is no fs/cwd/env, so the search is meaningless — skip it
                // entirely. Symmetric with the inventory gate above.
                // Explicit `.lockfile(path)` still runs on every target and
                // surfaces an error loudly per the doctrine in the module
                // header.
                #[cfg(not(target_arch = "wasm32"))]
                {
                    if let Some(path) = resolve_lockfile_auto() {
                        w.load_lockfile(&path)?;
                    } else {
                        w.try_load_lockfile_cwd()?;
                    }
                }
            }
            LockfileSource::Explicit(p) => {
                w.load_lockfile(&p)?;
            }
            LockfileSource::Disabled => {}
        }
        Ok(w)
    }
}

/// Returns the env-var override path if `WAFER_LOCKFILE` is set and
/// non-empty. `None` falls through to the default `./wafer.lock`
/// behaviour (which is silent if missing).
#[cfg(not(target_arch = "wasm32"))]
fn resolve_lockfile_auto() -> Option<PathBuf> {
    match std::env::var("WAFER_LOCKFILE") {
        Ok(s) if !s.is_empty() => Some(PathBuf::from(s)),
        _ => None,
    }
}
