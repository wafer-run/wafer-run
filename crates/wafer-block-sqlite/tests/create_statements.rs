//! A create into a table the executor has already written to is one
//! statement — the `INSERT` — once the table's schema is cached, counted by
//! SQLite's own statement trace.
//!
//! The executor asks where a created row's `id` comes from before minting
//! one. That probe used to answer "mint" both for a table with a text `id`
//! and for a missing table, so the answer was cached only once something
//! else had proven the table exists: the first two creates into an
//! insert-only table each paid the probe. On Cloudflare D1 each statement is
//! a network round trip.

use std::{collections::HashMap, sync::Mutex};

use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::interfaces::database::service::{pk, Column, DataType, DatabaseService, Table};

/// Every statement the service sent SQLite, as its trace reported it. One
/// test in this binary, so nothing else writes here.
static STATEMENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// SQLite also traces the statements it runs inside one (a `pragma_*`
/// table-valued function's `PRAGMA`), prefixed `-- `; those are not
/// statements the service sent, so they are not counted.
fn trace(sql: &str) {
    if !sql.starts_with("-- ") {
        STATEMENTS.lock().unwrap().push(sql.to_string());
    }
}

/// Take the statements traced so far, leaving the log empty.
fn drain_statements() -> Vec<String> {
    std::mem::take(&mut *STATEMENTS.lock().unwrap())
}

const TABLE: &str = "login_events";

fn event(n: u32) -> HashMap<String, serde_json::Value> {
    HashMap::from([("user_id".to_string(), serde_json::json!(format!("u{n}")))])
}

#[tokio::test]
async fn the_second_create_into_an_insert_only_table_is_one_insert() {
    // Strict (how a deployment with migrations runs, D1 included) and
    // non-strict schema take different probes on the first create; both must
    // have cached everything a create needs by the second.
    for strict in [true, false] {
        let mut conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        conn.trace(Some(trace));
        let svc = SQLiteDatabaseService::new(conn);
        svc.ensure_schema_table(&Table {
            name: TABLE.to_string(),
            columns: vec![
                pk("id"),
                Column::new("user_id", DataType::String),
                Column::new("created_at", DataType::Text),
                Column::new("updated_at", DataType::Text),
            ],
            indexes: Vec::new(),
            primary_key: Vec::new(),
            unique_keys: Vec::new(),
        })
        .await
        .expect("create login_events");
        svc.set_strict_schema(strict);
        drain_statements();

        svc.create(TABLE, event(1)).await.expect("first create");
        let first = drain_statements();
        assert!(
            first.len() > 1,
            "strict={strict}: the first create probes the schema: {first:#?}"
        );

        svc.create(TABLE, event(2)).await.expect("second create");
        let second = drain_statements();
        assert_eq!(
            second.len(),
            1,
            "strict={strict}: the second create is the insert alone: {second:#?}"
        );
        assert!(
            second[0].starts_with("INSERT"),
            "strict={strict}: {second:#?}"
        );
    }
}
