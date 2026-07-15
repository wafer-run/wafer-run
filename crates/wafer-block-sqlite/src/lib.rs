//! SQLite database service implementation for WAFER.
//!
//! Provides `SQLiteDatabaseService` implementing `DatabaseService`.
//! The trait is defined in `wafer_core::interfaces::database`.
//!
//! Use `wafer_core::service_blocks::database::register_with()` to register.

#![warn(missing_docs)]

/// SQLite-backed implementation of the `wafer-core` `DatabaseService`
/// interface. Exposes `SQLiteDatabaseService`, which dispatches the
/// schemaless CRUD/list/aggregate ops to dedicated connection-worker
/// threads (a write worker plus read-only readers for file-backed DBs)
/// so SQLite I/O never blocks an async executor thread.
pub mod service;

/// Dedicated connection-worker thread shared by the database and vector
/// services (PERF-02): async callers queue closures; SQLite I/O never runs
/// on an executor thread.
mod worker;

/// Register the `sqlite-vec` extension with SQLite's auto-extension list
/// the first time this is called. Safe to call repeatedly — after the
/// first successful registration, subsequent calls are no-ops.
///
/// The extension is registered globally via `sqlite3_auto_extension`, so
/// **every** `rusqlite::Connection` opened after this call will
/// automatically have the `vec0` virtual table and `vec_*` functions
/// available. The `conn` argument is accepted for API symmetry and
/// future flexibility but is not actually required for registration.
///
/// This mirrors the usage pattern shown in the `sqlite-vec` crate's
/// own tests (see `sqlite-vec-0.1.9/src/lib.rs`).
#[cfg(feature = "vectors")]
pub(crate) fn ensure_vec_loaded(_conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    use std::sync::OnceLock;
    static LOADED: OnceLock<()> = OnceLock::new();
    if LOADED.get().is_some() {
        return Ok(());
    }
    // SAFETY: `sqlite3_vec_init` is a C entry point exported by the
    // sqlite-vec C library with the standard SQLite extension init
    // signature `int (*)(sqlite3*, char**, const sqlite3_api_routines*)`.
    // The crate's Rust binding declares it as a parameterless `extern "C" fn()`,
    // so we must transmute to the fully-typed signature that
    // `rusqlite::ffi::sqlite3_auto_extension` expects. This registration
    // approach mirrors the pattern used in sqlite-vec's own test suite.
    // The static `OnceLock` guarantees we only register the function
    // pointer once per process — calling `sqlite3_auto_extension` twice
    // with the same pointer is a no-op in SQLite, but skipping it is
    // cheaper and clearer.
    type ExtInit = unsafe extern "C" fn(
        *mut rusqlite::ffi::sqlite3,
        *mut *mut std::os::raw::c_char,
        *const rusqlite::ffi::sqlite3_api_routines,
    ) -> std::os::raw::c_int;
    unsafe {
        let init: ExtInit = std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ());
        let rc = rusqlite::ffi::sqlite3_auto_extension(Some(init));
        if rc != rusqlite::ffi::SQLITE_OK {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rc),
                Some("failed to register sqlite-vec auto-extension".to_string()),
            ));
        }
    }
    let _ = LOADED.set(());
    Ok(())
}

#[cfg(feature = "vectors")]
pub mod vector;

#[cfg(all(test, feature = "vectors"))]
mod vectors_smoke_tests {
    use super::ensure_vec_loaded;

    /// Registering sqlite-vec as an auto-extension must make
    /// `vec_version()` available on a freshly opened connection.
    #[test]
    fn vec_version_available_after_ensure_loaded() {
        // Register once — must return Ok and be idempotent.
        let probe = rusqlite::Connection::open_in_memory().expect("open probe");
        ensure_vec_loaded(&probe).expect("first registration");
        ensure_vec_loaded(&probe).expect("second registration is a no-op");

        // Any connection opened after registration gets the extension.
        let conn = rusqlite::Connection::open_in_memory().expect("open conn");
        let version: String = conn
            .query_row("select vec_version()", [], |row| row.get(0))
            .expect("vec_version() should be registered");
        assert!(
            version.starts_with('v'),
            "expected sqlite-vec version string starting with 'v', got: {version:?}"
        );
    }
}
