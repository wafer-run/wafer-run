//! Runs the shared, backend-agnostic [`DatabaseService`] conformance suite
//! against the SQLite backend.
//!
//! This is the reference invocation of
//! [`wafer_core::interfaces::database::conformance::run_conformance`]. The same
//! call is what impresspress's D1 and browser-WASM adapters use to prove they
//! have not drifted from the native backends — if any op there silently
//! no-ops or fails open, the corresponding assertion in the shared suite
//! fails.
//!
//! [`DatabaseService`]: wafer_core::interfaces::database::service::DatabaseService

use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::interfaces::database::conformance::{
    run_conformance, run_two_instance_conformance,
};

/// The SQLite `DatabaseService` implementation must satisfy every op in the
/// shared conformance suite.
#[tokio::test]
async fn sqlite_database_service_is_conformant() {
    let svc = SQLiteDatabaseService::open_in_memory().expect("open in-memory sqlite");
    run_conformance(&svc).await;
}

/// Unique on-disk DB path; minimal tempfile stand-in (no new dev-dep) — the
/// same pattern `wafer-block-sqlite`'s own `service.rs` unit tests use for
/// their file-backed cases.
fn tempdb_path(tag: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!(
        "wafer-sqlite-conformance-{tag}-{}-{nonce}.db",
        std::process::id()
    ))
}

/// Same suite, same assertions, but against a **file-backed** service with
/// real read-only reader connections — the split-connection configuration
/// every production file-backed deployment runs, and the one
/// `sqlite_database_service_is_conformant` above never exercises:
/// `open_in_memory()` opens zero readers, so every "read" op there falls
/// back to the write worker and the read/write split never actually
/// happens. That gap is exactly how the `take_where` read/write-path bug
/// (`DELETE … RETURNING` dispatched through the read-only path) shipped
/// with a fully green conformance suite — the suite was never run against
/// the configuration the bug lived in. Asserting `reader_count() > 0`
/// guards against this test silently degrading back to that gap (e.g. if
/// reader connections fail to open in some environment).
#[tokio::test]
async fn sqlite_database_service_is_conformant_file_backed() {
    let path = tempdb_path("conformance");
    let svc = SQLiteDatabaseService::open(path.to_str().unwrap()).expect("open file-backed sqlite");
    assert!(
        svc.reader_count() > 0,
        "file-backed service must open reader workers, or this test exercises \
         the same single-connection configuration as the in-memory suite above"
    );
    run_conformance(&svc).await;
    let _ = std::fs::remove_file(&path);
}

/// Two services opened on one file — two processes' worth of connections and
/// schema caches — must not hide each other's schema changes: a table one
/// saw missing and the other created is visible to the first.
#[tokio::test]
async fn two_sqlite_services_on_one_file_see_each_others_tables() {
    let path = tempdb_path("two-instance");
    let a = SQLiteDatabaseService::open(path.to_str().unwrap()).expect("open first service");
    let b = SQLiteDatabaseService::open(path.to_str().unwrap()).expect("open second service");
    run_two_instance_conformance(&a, &b).await;
    drop((a, b));
    let _ = std::fs::remove_file(&path);
}

/// An `id` key that is not SQLite's rowid alias is an ordinary column, which
/// a rowid table lets hold `NULL`: `create` without an id must not leave the
/// row keyed `NULL` and report SQLite's internal rowid as its id. Covers the
/// declarations whose type merely contains `INT` (`INT`, `BIGINT`), a
/// composite key and a `WITHOUT ROWID` table — integer ids nothing fills, so
/// a row without one is refused and nothing is written — against the one
/// that is the alias (`INTEGER PRIMARY KEY`, numbered by SQLite) and a text
/// key (a minted id).
#[tokio::test]
async fn only_the_rowid_alias_numbers_its_own_rows() {
    use std::collections::HashMap;

    use wafer_core::interfaces::database::service::{DatabaseError, DatabaseService};

    let svc = SQLiteDatabaseService::open_in_memory().expect("open in-memory sqlite");
    let caller_supplied = [
        (
            "t_int",
            "CREATE TABLE t_int (id INT PRIMARY KEY, name TEXT)",
        ),
        (
            "t_bigint",
            "CREATE TABLE t_bigint (id BIGINT PRIMARY KEY, name TEXT)",
        ),
        (
            "t_composite",
            "CREATE TABLE t_composite (id INTEGER, name TEXT, PRIMARY KEY (id, name))",
        ),
        (
            "t_no_rowid",
            "CREATE TABLE t_no_rowid (id INTEGER PRIMARY KEY, name TEXT) WITHOUT ROWID",
        ),
    ];
    let filled = [
        (
            "t_alias",
            "CREATE TABLE t_alias (id INTEGER PRIMARY KEY, name TEXT)",
        ),
        (
            "t_text",
            "CREATE TABLE t_text (id TEXT PRIMARY KEY, name TEXT)",
        ),
    ];
    for (_, ddl) in caller_supplied.iter().chain(&filled) {
        svc.exec_raw(ddl, &[]).await.expect(ddl);
    }
    let named = |table: &str| HashMap::from([("name".to_string(), serde_json::json!(table))]);
    let rows = |table: &'static str| {
        let svc = &svc;
        async move {
            svc.query_raw(&format!("SELECT id, name FROM {table}"), &[])
                .await
                .expect("query")
        }
    };

    for (table, _) in caller_supplied {
        let err = svc
            .create(table, named(table))
            .await
            .expect_err("an integer id nothing fills must be supplied");
        assert!(
            matches!(&err, DatabaseError::InvalidArgument(msg) if msg.contains(table)),
            "{table}: {err:?}"
        );
        assert!(rows(table).await.is_empty(), "{table}: nothing is written");
        let mut with_id = named(table);
        with_id.insert("id".to_string(), serde_json::json!(7));
        let created = svc.create(table, with_id).await.expect("a supplied id");
        let got = svc.get(table, &created.id).await.expect("get");
        assert_eq!(got.data["name"], serde_json::json!(table));
    }

    for (table, _) in filled {
        let created = svc
            .create(table, named(table))
            .await
            .unwrap_or_else(|e| panic!("create in {table}: {e}"));
        let got = svc
            .get(table, &created.id)
            .await
            .unwrap_or_else(|e| panic!("{table}: get({:?}): {e}", created.id));
        assert_eq!(got.data["name"], serde_json::json!(table));
        if table == "t_alias" {
            assert_eq!(
                created.data["id"],
                serde_json::json!(1),
                "SQLite numbers the alias"
            );
        } else {
            assert!(
                created.data["id"].is_string(),
                "a minted id: {:?}",
                created.data
            );
        }
    }
}

/// The repair the CHANGELOG gives for an `id INT PRIMARY KEY` table written
/// by the executor that took any `INT` key for the rowid alias: those rows
/// hold a `NULL` id, and `create` reported each row's rowid as its id. Setting
/// the id to the rowid makes the ids callers were given find their rows.
#[tokio::test]
async fn the_changelog_repair_gives_null_id_rows_the_ids_callers_were_given() {
    use wafer_core::interfaces::database::service::DatabaseService;

    let svc = SQLiteDatabaseService::open_in_memory().expect("open in-memory sqlite");
    svc.exec_raw("CREATE TABLE legacy (id INT PRIMARY KEY, name TEXT)", &[])
        .await
        .expect("create");
    // What the old `create` wrote, and the ids it returned (the rowids).
    for name in ["first", "second"] {
        svc.exec_raw(&format!("INSERT INTO legacy (name) VALUES ('{name}')"), &[])
            .await
            .expect("a legacy row");
    }
    let before = svc
        .query_raw("SELECT COUNT(*) AS n FROM legacy WHERE id IS NULL", &[])
        .await
        .expect("count");
    assert_eq!(
        before[0].data["n"],
        serde_json::json!(2),
        "the detection query"
    );

    svc.exec_raw("UPDATE legacy SET id = rowid WHERE id IS NULL", &[])
        .await
        .expect("the repair");
    for (id, name) in [("1", "first"), ("2", "second")] {
        let got = svc
            .get("legacy", id)
            .await
            .expect("the id the caller was given");
        assert_eq!(got.data["name"], serde_json::json!(name));
    }
}
