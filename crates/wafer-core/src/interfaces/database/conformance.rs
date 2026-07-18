//! Backend-agnostic conformance suite for [`DatabaseService`].
//!
//! Every backend that implements [`DatabaseService`] — the native SQLite and
//! PostgreSQL services, plus the out-of-tree D1 / browser-WASM adapters that
//! impresspress registers — must behave identically for the same op. When an
//! adapter silently drops or no-ops an op, the observable result changes
//! (e.g. a rate-limit counter that never increments, so limiting *fails
//! open*). No amount of per-backend unit testing catches that class of drift,
//! because each backend only tests itself.
//!
//! [`run_conformance`] closes the gap: it drives **every** method on the
//! trait against a live service, asserting the concrete observable behavior
//! (round-trips, counts, filtered/sorted/paginated results, atomic increments,
//! insert-vs-update upserts, grouped aggregates, raw SQL, schema management).
//! The assertions are strong enough that a no-op or fail-open implementation
//! of any single op fails a test rather than passing silently.
//!
//! # Reuse (the anti-drift mechanism)
//!
//! The suite is `pub` and depends only on the trait plus the plain-data query
//! types — no runtime, no executor, no `tokio`. Any crate that owns a
//! `DatabaseService` implementation calls it from its own test with whatever
//! async harness it already uses:
//!
//! ```no_run
//! # async fn wiring() {
//! // wafer-block-sqlite (native, tokio):
//! //   let svc = SQLiteDatabaseService::open_in_memory().unwrap();
//! //   wafer_core::interfaces::database::conformance::run_conformance(&svc).await;
//! //
//! // impresspress D1 / browser adapter (wasm32, wasm_bindgen_test):
//! //   let svc = D1DatabaseService::new(env_binding);
//! //   run_conformance(&svc).await;
//! # }
//! ```
//!
//! To enable the suite, turn on `wafer-core`'s `conformance` feature in the
//! consuming crate's `[dev-dependencies]` (see `wafer-block-sqlite`'s manifest
//! for the pattern). The feature is off by default, so the harness is never
//! compiled into a production build.
//!
//! # Contract
//!
//! - The service is driven through `&dyn DatabaseService` only; the suite
//!   never reaches behind the trait, so it is genuinely backend-agnostic.
//! - Failures `panic!` (via `assert!`), which every test framework — native
//!   `#[tokio::test]` and `wasm_bindgen_test` alike — reports as a failure.
//! - All tables are named `conf_*` and dropped-then-created at the start of
//!   the section that uses them, so the suite is safe to re-run against a
//!   **persistent** database (D1) without leftover state.
//! - Raw SQL passed to [`DatabaseService::query_raw`] / [`exec_raw`] carries
//!   its values as inline literals (never bind placeholders), because the
//!   placeholder dialect (`?` vs `$1`) differs by backend and the suite has no
//!   backend handle. The literals are all suite-controlled constants.
//!
//! # Backend divergences this suite surfaced (PostgreSQL)
//!
//! Running the suite against a live PostgreSQL server (the gated
//! `wafer-block-postgres` test) was the first live-DB exercise of that backend,
//! and it exposed four real defects that SQLite's dynamic typing had hidden.
//! Three are now fixed at the shared renderer / decoder layer, and this suite
//! exercises the exact shapes that tripped them so it regression-guards each;
//! the fourth is deferred (it does not manifest for any real code):
//!
//! 2. **`sum` over an `INT` column (FIXED).** The top-level `sum` op decodes its
//!    scalar as `f64`, but Postgres `COALESCE(SUM(int), 0)` returned `INT8`,
//!    which the `f64` decode rejected. The `COALESCE` fallback is now
//!    floating-point, so the result resolves to `DOUBLE PRECISION`.
//!    `check_count_and_sum` sums the integer `score` column to cover this.
//! 3. **`upsert` `WindowedCounter` ambiguous column (FIXED).** The `ON CONFLICT
//!    DO UPDATE SET` CASE expressions referenced the counter/window columns
//!    unqualified, which Postgres rejected as `column reference … is
//!    ambiguous`. The builder now qualifies them with the target table. This is
//!    the rate-limiter path, so the fix matters.
//! 4. **`aggregate` `CaseWhenSum` NUMERIC silent-NULL (FIXED).** `SUM(CASE WHEN …
//!    THEN 1 ELSE 0 END)` summed bound `BIGINT` literals, so Postgres returned
//!    `NUMERIC`, which `row_to_record` could not decode as `f64` and *silently
//!    dropped to `NULL`* — a silent wrong result. The builder now emits inline
//!    `INT4` literals (so the SUM is `INT8`), the decoder now decodes `NUMERIC`
//!    proper, and an undecodable value now hard-errors instead of NULLing.
//!
//! 1. **Timestamp string vs `TIMESTAMPTZ` (DEFERRED — does not manifest).** The
//!    shared `create`/`update` path (`stamp_timestamps` in `exec.rs`)
//!    auto-stamps `created_at`/`updated_at` as an RFC3339 *string*. A real
//!    Postgres `TIMESTAMPTZ` column (declared via `wafer_schema::timestamps()`)
//!    rejects that bound text parameter. But **no block in the workspace uses a
//!    `TIMESTAMPTZ` timestamp column** — they all hand-write TEXT columns
//!    holding one canonical RFC3339 string so `expires_at < cutoff` string
//!    comparisons work, and TEXT accepts the stamped string fine. Fixing this
//!    correctly needs a column-type-aware bind (the value-driven shortcut would
//!    break the TEXT convention — see the `generate_bind!` note in the Postgres
//!    service), so it is deferred and this suite keeps TEXT timestamps.
//!
//! [`exec_raw`]: DatabaseService::exec_raw

use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp, FilterTree, ListOptions, SortField};

use super::service::{
    pk, AggregateColumnSpec, AggregateSpec, Column, DataType, DatabaseError, DatabaseService,
    GroupBySpec, Record, Table, UpsertConflict, UpsertSpec,
};

// ---------------------------------------------------------------------------
// Small construction / extraction helpers
// ---------------------------------------------------------------------------

/// Build a flat equality [`Filter`] on `field`.
fn eq(field: &str, value: serde_json::Value) -> Filter {
    Filter {
        field: field.to_string(),
        operator: FilterOp::Equal,
        value,
    }
}

/// Build a [`Filter`] with an explicit operator.
fn filt(field: &str, operator: FilterOp, value: serde_json::Value) -> Filter {
    Filter {
        field: field.to_string(),
        operator,
        value,
    }
}

/// Build a row payload from `(key, value)` pairs.
fn row(
    pairs: impl IntoIterator<Item = (&'static str, serde_json::Value)>,
) -> HashMap<String, serde_json::Value> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// Extract an `i64` from a record field, panicking with context on the wrong
/// shape — a backend that returns text where a number is expected is drift.
fn field_i64(rec: &Record, key: &str) -> i64 {
    rec.data
        .get(key)
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_else(|| panic!("field {key:?} is not an integer: {:?}", rec.data.get(key)))
}

/// Extract an `f64` from a record field (integers decode fine as floats).
fn field_f64(rec: &Record, key: &str) -> f64 {
    rec.data
        .get(key)
        .and_then(serde_json::Value::as_f64)
        .unwrap_or_else(|| panic!("field {key:?} is not numeric: {:?}", rec.data.get(key)))
}

/// A `Table` whose columns cover the shapes the CRUD/list/aggregate checks
/// need: a text PK, text/int payload columns, a nullable column (for
/// NULL-predicate coverage), and timestamps (for date-bucket grouping).
///
/// `created_at`/`updated_at` are `Text`, not `DateTime`, deliberately — and
/// this mirrors how every block in the workspace actually stores timestamps: a
/// single canonical RFC3339 *string* in a TEXT column (see impresspress
/// `auth/repo/mod.rs::now_iso`), so `expires_at < cutoff`-style string
/// comparisons work. The shared `create`/`update` path auto-stamps them with an
/// RFC3339 string (`stamp_timestamps` in `exec.rs`), which a TEXT column stores
/// verbatim on every backend, and the Postgres date-bucket expression casts the
/// text to a date so grouping still works.
///
/// A block declaring a real `TIMESTAMPTZ` column (via
/// `wafer_schema::timestamps()`) would need a column-type-aware bind for the
/// stamped string — a deferred follow-up (see the `generate_bind!` note in the
/// Postgres service). No block does this today, so the suite stays on TEXT
/// timestamps rather than exercising an unfixed path.
fn crud_table(name: &str) -> Table {
    Table {
        name: name.to_string(),
        columns: vec![
            pk("id"),
            Column::new("name", DataType::Text).null(),
            Column::new("category", DataType::Text).null(),
            // `score` (INT) drives count/filter/increment (the CAS counter) and
            // the integer-column `sum` check (`SUM(<int>)` returns Postgres
            // `INT8`, which the `f64` sum decode must accept).
            Column::new("score", DataType::Int).null(),
            // `amount` (FLOAT) drives the aggregate Sum/Avg/Max checks, whose
            // fractional means need a floating-point column.
            Column::new("amount", DataType::Float).null(),
            Column::new("note", DataType::Text).null(),
            Column::new("created_at", DataType::Text).null(),
            Column::new("updated_at", DataType::Text).null(),
        ],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    }
}

/// Drop `table` if present, then (re)create it — leaving a known-empty table
/// even on a persistent database that a prior run populated.
async fn reset(svc: &dyn DatabaseService, table: &Table) {
    svc.schema_drop_table(&table.name)
        .await
        .expect("schema_drop_table (idempotent) must succeed");
    svc.ensure_schema_table(table)
        .await
        .expect("ensure_schema_table must succeed");
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Drive every [`DatabaseService`] op against `svc` and assert correct
/// observable behavior. Panics (fails the test) on the first divergence.
///
/// Covers, in order: schema management (`ensure_schema_table[s]`,
/// `schema_table_exists`, `schema_add_column`, `schema_drop_table`,
/// `set_strict_schema`); `create`/`get`; `count`/`sum` across the full
/// [`FilterOp`] surface; `list` (filter, sort, limit, offset, projection,
/// OR-group `filter_tree`, `total_count`); `update`/`update_where`/
/// `update_where_count`; `delete`/`delete_where`/`delete_where_count`/
/// `take_where`; `increment_field_where` (atomic CAS bump + decrement);
/// `upsert` (`SetColumns` insert-then-update and the rate-limiter
/// `WindowedCounter`); `aggregate` (grouped `Count`/`Sum`/`Avg`/`Max`,
/// `CaseWhenSum`, and `DateBucket`); and `query_raw`/`exec_raw`.
pub async fn run_conformance(svc: &dyn DatabaseService) {
    // Exercise the (sync, default-no-op) strict-schema toggle and pin the
    // service into non-strict mode so the suite's explicit schemas drive the
    // behavior. Strict-mode *semantics* are backend-specific (a no-op on
    // adapters that don't cache a schema), so they aren't asserted here.
    svc.set_strict_schema(false);

    check_schema_management(svc).await;
    check_create_get(svc).await;
    check_count_and_sum(svc).await;
    check_list(svc).await;
    check_update_family(svc).await;
    check_delete_family(svc).await;
    check_take_where(svc).await;
    check_increment(svc).await;
    check_upsert_set_columns(svc).await;
    check_upsert_windowed_counter(svc).await;
    check_aggregate(svc).await;
    check_raw_sql(svc).await;
}

// ---------------------------------------------------------------------------
// Shared read fixture
// ---------------------------------------------------------------------------

/// Reset `conf_read` and seed it with five deterministic, immutable rows.
/// Read-only checks (`count`/`sum`/`list`/`aggregate`/`query_raw`) share it;
/// nothing mutates it after this returns.
async fn seed_read_fixture(svc: &dyn DatabaseService) {
    let table = crud_table("conf_read");
    reset(svc, &table).await;

    // (id, name, category, score, amount, note, created_at). `amount` mirrors
    // `score` so the sum/aggregate expectations stay easy to read. `created_at`
    // is stored in a TEXT column, and the date-bucket check (which casts the
    // text to a date on Postgres) groups these into the Jan-15 and Jan-16
    // buckets.
    let rows = [
        ("r1", "alpha", "x", 10, Some("hi"), "2026-01-15"),
        ("r2", "bravo", "x", 20, None, "2026-01-15"),
        ("r3", "charlie", "y", 5, None, "2026-01-16"),
        ("r4", "delta", "y", 5, None, "2026-01-16"),
        ("r5", "echo", "z", 100, None, "2026-01-16"),
    ];
    for (id, name, category, score, note, created) in rows {
        let mut data = row([
            ("id", serde_json::json!(id)),
            ("name", serde_json::json!(name)),
            ("category", serde_json::json!(category)),
            ("score", serde_json::json!(score)),
            ("amount", serde_json::json!(f64::from(score))),
            ("created_at", serde_json::json!(created)),
        ]);
        if let Some(n) = note {
            data.insert("note".to_string(), serde_json::json!(n));
        }
        let created_rec = svc
            .create("conf_read", data)
            .await
            .expect("create must succeed");
        assert_eq!(created_rec.id, id, "create must preserve the supplied id");
    }
}

// ---------------------------------------------------------------------------
// create / get
// ---------------------------------------------------------------------------

async fn check_create_get(svc: &dyn DatabaseService) {
    seed_read_fixture(svc).await;

    // create → get round-trips every stored field.
    let got = svc.get("conf_read", "r1").await.expect("get r1");
    assert_eq!(got.id, "r1");
    assert_eq!(got.data["name"], serde_json::json!("alpha"));
    assert_eq!(got.data["category"], serde_json::json!("x"));
    assert_eq!(field_i64(&got, "score"), 10);
    assert_eq!(got.data["note"], serde_json::json!("hi"));

    // A create without an explicit id must synthesize one and still round-trip.
    let auto = svc
        .create(
            "conf_read",
            row([
                ("name", serde_json::json!("auto")),
                ("category", serde_json::json!("gen")),
                ("score", serde_json::json!(1)),
            ]),
        )
        .await
        .expect("create without id");
    assert!(
        !auto.id.is_empty(),
        "backend must synthesize a non-empty id"
    );
    let reread = svc
        .get("conf_read", &auto.id)
        .await
        .expect("get generated id");
    assert_eq!(reread.data["name"], serde_json::json!("auto"));

    // Nullable column with no value supplied reads back as JSON null.
    let r2 = svc.get("conf_read", "r2").await.expect("get r2");
    assert_eq!(
        r2.data.get("note"),
        Some(&serde_json::Value::Null),
        "unset nullable column must be NULL, not absent or a wrong default"
    );

    // Missing id is a typed NotFound, never an Ok(empty) fail-open.
    let err = svc
        .get("conf_read", "does-not-exist")
        .await
        .expect_err("missing row must error");
    assert!(
        matches!(err, DatabaseError::NotFound),
        "expected NotFound, got: {err:?}"
    );

    // Remove the extra auto row so the shared fixture is back to five rows.
    svc.delete("conf_read", &auto.id)
        .await
        .expect("cleanup auto row");
    assert_eq!(
        svc.count("conf_read", &[]).await.expect("count"),
        5,
        "fixture must hold exactly five rows for downstream read checks"
    );
}

// ---------------------------------------------------------------------------
// count / sum — full FilterOp surface
// ---------------------------------------------------------------------------

async fn check_count_and_sum(svc: &dyn DatabaseService) {
    let count = |f: Vec<Filter>| async move { svc.count("conf_read", &f).await.expect("count") };

    assert_eq!(count(vec![]).await, 5, "unfiltered count");
    assert_eq!(
        count(vec![eq("category", serde_json::json!("x"))]).await,
        2,
        "Equal"
    );
    assert_eq!(
        count(vec![filt(
            "category",
            FilterOp::NotEqual,
            serde_json::json!("x")
        )])
        .await,
        3,
        "NotEqual"
    );
    assert_eq!(
        count(vec![filt(
            "score",
            FilterOp::GreaterThan,
            serde_json::json!(10)
        )])
        .await,
        2,
        "GreaterThan (20, 100)"
    );
    assert_eq!(
        count(vec![filt(
            "score",
            FilterOp::GreaterEqual,
            serde_json::json!(10)
        )])
        .await,
        3,
        "GreaterEqual (10, 20, 100)"
    );
    assert_eq!(
        count(vec![filt(
            "score",
            FilterOp::LessThan,
            serde_json::json!(10)
        )])
        .await,
        2,
        "LessThan (5, 5)"
    );
    assert_eq!(
        count(vec![filt(
            "score",
            FilterOp::LessEqual,
            serde_json::json!(5)
        )])
        .await,
        2,
        "LessEqual (5, 5)"
    );
    assert_eq!(
        count(vec![filt("name", FilterOp::Like, serde_json::json!("a%"))]).await,
        1,
        "Like 'a%' matches only alpha"
    );
    assert_eq!(
        count(vec![filt(
            "category",
            FilterOp::In,
            serde_json::json!(["x", "z"])
        )])
        .await,
        3,
        "In [x, z] (r1, r2, r5)"
    );
    assert_eq!(
        count(vec![filt(
            "note",
            FilterOp::IsNull,
            serde_json::Value::Null
        )])
        .await,
        4,
        "IsNull note (all but r1)"
    );
    assert_eq!(
        count(vec![filt(
            "note",
            FilterOp::IsNotNull,
            serde_json::Value::Null
        )])
        .await,
        1,
        "IsNotNull note (only r1)"
    );

    // sum over a numeric column, filtered and unfiltered.
    let sum_all = svc.sum("conf_read", "amount", &[]).await.expect("sum all");
    assert!(
        (sum_all - 140.0).abs() < 1e-9,
        "sum of all amounts == 140, got {sum_all}"
    );
    let sum_x = svc
        .sum(
            "conf_read",
            "amount",
            &[eq("category", serde_json::json!("x"))],
        )
        .await
        .expect("sum x");
    assert!(
        (sum_x - 30.0).abs() < 1e-9,
        "sum of category=x amounts == 30, got {sum_x}"
    );

    // sum over an INTEGER column. `SUM(<int>)` returns Postgres `INT8`, which
    // the `f64` scalar decode must accept — the top-level `sum` op always yields
    // an `f64` regardless of the summed column's type. (SQLite's loose typing
    // hides this; a live Postgres run does not.)
    let sum_score = svc.sum("conf_read", "score", &[]).await.expect("sum score");
    assert!(
        (sum_score - 140.0).abs() < 1e-9,
        "sum of all scores == 140, got {sum_score}"
    );
    let sum_score_y = svc
        .sum(
            "conf_read",
            "score",
            &[eq("category", serde_json::json!("y"))],
        )
        .await
        .expect("sum score y");
    assert!(
        (sum_score_y - 10.0).abs() < 1e-9,
        "sum of category=y scores == 10, got {sum_score_y}"
    );

    // count/sum on a missing table must fail-safe to zero, not error.
    assert_eq!(
        svc.count("conf_absent_table", &[])
            .await
            .expect("count missing table"),
        0,
        "count on a non-existent table returns 0"
    );
}

// ---------------------------------------------------------------------------
// list — filter / sort / limit / offset / projection / filter_tree / total
// ---------------------------------------------------------------------------

async fn check_list(svc: &dyn DatabaseService) {
    // Flat filter + multi-key sort; total_count reflects the FILTERED set.
    let opts = ListOptions {
        filters: vec![eq("category", serde_json::json!("y"))],
        sort: vec![
            SortField {
                field: "score".into(),
                desc: false,
            },
            SortField {
                field: "name".into(),
                desc: false,
            },
        ],
        limit: 10,
        ..Default::default()
    };
    let listed = svc.list("conf_read", &opts).await.expect("list y");
    let names: Vec<&str> = listed
        .records
        .iter()
        .map(|r| r.data["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["charlie", "delta"], "filtered + sorted rows");
    assert_eq!(
        listed.total_count, 2,
        "total_count must be the filtered count, not the full table"
    );

    // Sort desc + limit.
    let top2 = svc
        .list(
            "conf_read",
            &ListOptions {
                sort: vec![SortField {
                    field: "score".into(),
                    desc: true,
                }],
                limit: 2,
                ..Default::default()
            },
        )
        .await
        .expect("list top2");
    let top_scores: Vec<i64> = top2.records.iter().map(|r| field_i64(r, "score")).collect();
    assert_eq!(top_scores, vec![100, 20], "descending score, limited to 2");
    assert_eq!(top2.records.len(), 2, "limit honored");
    assert_eq!(
        top2.total_count, 5,
        "total_count is the full unpaginated size when there is no filter"
    );

    // offset paginates past the first page.
    let page2 = svc
        .list(
            "conf_read",
            &ListOptions {
                sort: vec![SortField {
                    field: "score".into(),
                    desc: true,
                }],
                limit: 2,
                offset: 1,
                ..Default::default()
            },
        )
        .await
        .expect("list page2");
    let page2_scores: Vec<i64> = page2
        .records
        .iter()
        .map(|r| field_i64(r, "score"))
        .collect();
    assert_eq!(page2_scores, vec![20, 10], "offset=1 skips the top row");

    // Column projection: only the requested columns come back.
    let projected = svc
        .list(
            "conf_read",
            &ListOptions {
                filters: vec![eq("id", serde_json::json!("r1"))],
                columns: Some(vec!["id".into(), "name".into()]),
                ..Default::default()
            },
        )
        .await
        .expect("list projected");
    let prow = &projected.records[0].data;
    assert!(prow.contains_key("name"), "projected column present");
    assert!(!prow.contains_key("score"), "unprojected column absent");
    assert!(!prow.contains_key("category"), "unprojected column absent");

    // OR-group filter_tree must actually execute (not flatten to "match all").
    let tree = vec![FilterTree::Any(vec![
        FilterTree::Leaf(eq("category", serde_json::json!("x"))),
        FilterTree::Leaf(eq("category", serde_json::json!("z"))),
    ])];
    let grouped = svc
        .list(
            "conf_read",
            &ListOptions {
                filter_tree: Some(tree),
                sort: vec![SortField {
                    field: "name".into(),
                    desc: false,
                }],
                ..Default::default()
            },
        )
        .await
        .expect("list grouped");
    let grouped_names: Vec<&str> = grouped
        .records
        .iter()
        .map(|r| r.data["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        grouped_names,
        vec!["alpha", "bravo", "echo"],
        "OR group returns only x|z rows, not all five"
    );
    assert_eq!(
        grouped.total_count, 3,
        "total_count must apply the group predicate too"
    );

    // list on a missing table is empty, not an error.
    let empty = svc
        .list("conf_absent_table", &ListOptions::default())
        .await
        .expect("list missing table");
    assert!(empty.records.is_empty());
    assert_eq!(empty.total_count, 0);
}

// ---------------------------------------------------------------------------
// update / update_where / update_where_count
// ---------------------------------------------------------------------------

async fn check_update_family(svc: &dyn DatabaseService) {
    let table = crud_table("conf_update");
    reset(svc, &table).await;
    for (id, category) in [("u1", "a"), ("u2", "a"), ("u3", "b")] {
        svc.create(
            "conf_update",
            row([
                ("id", serde_json::json!(id)),
                ("category", serde_json::json!(category)),
                ("name", serde_json::json!("orig")),
                ("score", serde_json::json!(1)),
            ]),
        )
        .await
        .expect("seed conf_update");
    }

    // Single-row update touches only its target.
    let updated = svc
        .update("conf_update", "u1", row([("score", serde_json::json!(9))]))
        .await
        .expect("update u1");
    assert_eq!(
        field_i64(&updated, "score"),
        9,
        "returned record reflects update"
    );
    assert_eq!(
        field_i64(
            &svc.get("conf_update", "u1").await.expect("get u1"),
            "score"
        ),
        9,
        "persisted update"
    );
    assert_eq!(
        field_i64(
            &svc.get("conf_update", "u2").await.expect("get u2"),
            "score"
        ),
        1,
        "sibling row untouched"
    );

    // update_where mutates every matching row.
    svc.update_where(
        "conf_update",
        &[eq("category", serde_json::json!("a"))],
        row([("name", serde_json::json!("X"))]),
    )
    .await
    .expect("update_where a");
    for id in ["u1", "u2"] {
        assert_eq!(
            svc.get("conf_update", id).await.expect("get").data["name"],
            serde_json::json!("X"),
            "category=a rows renamed"
        );
    }
    assert_eq!(
        svc.get("conf_update", "u3").await.expect("get u3").data["name"],
        serde_json::json!("orig"),
        "category=b row untouched by category=a update"
    );

    // update_where_count returns the affected-row count.
    let n = svc
        .update_where_count(
            "conf_update",
            &[eq("category", serde_json::json!("b"))],
            row([("name", serde_json::json!("Y"))]),
        )
        .await
        .expect("update_where_count b");
    assert_eq!(n, 1, "exactly one category=b row updated");
    assert_eq!(
        svc.get("conf_update", "u3").await.expect("get u3").data["name"],
        serde_json::json!("Y")
    );

    let none = svc
        .update_where_count(
            "conf_update",
            &[eq("category", serde_json::json!("nomatch"))],
            row([("name", serde_json::json!("Z"))]),
        )
        .await
        .expect("update_where_count no match");
    assert_eq!(none, 0, "no matching rows -> zero updated");
}

// ---------------------------------------------------------------------------
// delete / delete_where / delete_where_count
// ---------------------------------------------------------------------------

async fn check_delete_family(svc: &dyn DatabaseService) {
    let table = crud_table("conf_delete");
    reset(svc, &table).await;
    for (id, category) in [
        ("d1", "a"),
        ("d2", "a"),
        ("d3", "b"),
        ("d4", "b"),
        ("d5", "c"),
    ] {
        svc.create(
            "conf_delete",
            row([
                ("id", serde_json::json!(id)),
                ("category", serde_json::json!(category)),
            ]),
        )
        .await
        .expect("seed conf_delete");
    }

    // Delete by id.
    svc.delete("conf_delete", "d5").await.expect("delete d5");
    assert!(
        matches!(
            svc.get("conf_delete", "d5").await.expect_err("d5 gone"),
            DatabaseError::NotFound
        ),
        "deleted row must be NotFound"
    );
    assert_eq!(svc.count("conf_delete", &[]).await.expect("count"), 4);

    // delete_where removes the matching subset.
    svc.delete_where("conf_delete", &[eq("category", serde_json::json!("a"))])
        .await
        .expect("delete_where a");
    assert_eq!(
        svc.count("conf_delete", &[eq("category", serde_json::json!("a"))])
            .await
            .expect("count a"),
        0,
        "all category=a rows deleted"
    );
    assert_eq!(svc.count("conf_delete", &[]).await.expect("count"), 2);

    // delete_where_count returns the number removed.
    let removed = svc
        .delete_where_count("conf_delete", &[eq("category", serde_json::json!("b"))])
        .await
        .expect("delete_where_count b");
    assert_eq!(removed, 2, "two category=b rows removed");
    assert_eq!(svc.count("conf_delete", &[]).await.expect("count"), 0);

    let none = svc
        .delete_where_count("conf_delete", &[eq("category", serde_json::json!("gone"))])
        .await
        .expect("delete_where_count no match");
    assert_eq!(none, 0, "no matching rows -> zero removed");
}

// ---------------------------------------------------------------------------
// take_where (atomic select-and-delete)
// ---------------------------------------------------------------------------

async fn check_take_where(svc: &dyn DatabaseService) {
    let table = crud_table("conf_take");
    reset(svc, &table).await;
    for (id, category) in [("t1", "x"), ("t2", "x"), ("t3", "y")] {
        svc.create(
            "conf_take",
            row([
                ("id", serde_json::json!(id)),
                ("category", serde_json::json!(category)),
                ("name", serde_json::json!(id)),
            ]),
        )
        .await
        .expect("seed conf_take");
    }

    // take_where returns the removed rows (with data) AND deletes them.
    let taken = svc
        .take_where("conf_take", &[eq("category", serde_json::json!("x"))])
        .await
        .expect("take_where x");
    assert_eq!(taken.len(), 2, "both category=x rows returned");
    let mut taken_ids: Vec<&str> = taken.iter().map(|r| r.id.as_str()).collect();
    taken_ids.sort_unstable();
    assert_eq!(taken_ids, vec!["t1", "t2"], "returned rows carry their ids");
    assert_eq!(
        svc.count("conf_take", &[]).await.expect("count"),
        1,
        "taken rows are gone; only t3 remains"
    );

    // A second take of the same predicate is now empty (already taken).
    let again = svc
        .take_where("conf_take", &[eq("category", serde_json::json!("x"))])
        .await
        .expect("take_where x again");
    assert!(again.is_empty(), "nothing left to take");
}

// ---------------------------------------------------------------------------
// increment_field_where (atomic CAS bump)
// ---------------------------------------------------------------------------

async fn check_increment(svc: &dyn DatabaseService) {
    let table = Table {
        name: "conf_incr".to_string(),
        columns: vec![
            pk("id"),
            Column::new("score", DataType::Int).null(),
            Column::new("created_at", DataType::Text).null(),
            Column::new("updated_at", DataType::Text).null(),
        ],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    };
    reset(svc, &table).await;
    for (id, score) in [("i1", 0), ("i2", 0), ("i3", 10)] {
        svc.create(
            "conf_incr",
            row([
                ("id", serde_json::json!(id)),
                ("score", serde_json::json!(score)),
            ]),
        )
        .await
        .expect("seed conf_incr");
    }

    // CAS-style bump: the predicate (score < 10) is applied atomically in the
    // same UPDATE, so i3 (score 10) is excluded. A no-op override of this op
    // (the trait default returns an error) fails here immediately.
    let bumped = svc
        .increment_field_where(
            "conf_incr",
            "score",
            5,
            &[filt("score", FilterOp::LessThan, serde_json::json!(10))],
        )
        .await
        .expect("increment_field_where must be implemented");
    assert_eq!(bumped, 2, "only i1 and i2 match score < 10");
    assert_eq!(
        field_i64(&svc.get("conf_incr", "i1").await.unwrap(), "score"),
        5
    );
    assert_eq!(
        field_i64(&svc.get("conf_incr", "i2").await.unwrap(), "score"),
        5
    );
    assert_eq!(
        field_i64(&svc.get("conf_incr", "i3").await.unwrap(), "score"),
        10,
        "row failing the CAS predicate is untouched"
    );

    // Negative delta decrements a single targeted row.
    let dec = svc
        .increment_field_where(
            "conf_incr",
            "score",
            -5,
            &[eq("id", serde_json::json!("i1"))],
        )
        .await
        .expect("decrement");
    assert_eq!(dec, 1);
    assert_eq!(
        field_i64(&svc.get("conf_incr", "i1").await.unwrap(), "score"),
        0
    );

    // No match -> zero rows, no mutation.
    let none = svc
        .increment_field_where(
            "conf_incr",
            "score",
            1,
            &[eq("id", serde_json::json!("nope"))],
        )
        .await
        .expect("increment no match");
    assert_eq!(none, 0);
}

// ---------------------------------------------------------------------------
// upsert — SetColumns (insert then conflict-update)
// ---------------------------------------------------------------------------

async fn check_upsert_set_columns(svc: &dyn DatabaseService) {
    let table = Table {
        name: "conf_upsert".to_string(),
        columns: vec![pk("id"), Column::new("name", DataType::Text).null()],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    };
    reset(svc, &table).await;

    // First upsert: no existing id=w1 -> INSERT.
    let n1 = svc
        .upsert(
            "conf_upsert",
            UpsertSpec {
                data: vec![
                    ("id".into(), serde_json::json!("w1")),
                    ("name".into(), serde_json::json!("a")),
                ],
                conflict_columns: vec!["id".into()],
                on_conflict: UpsertConflict::SetColumns(vec!["name".into()]),
            },
        )
        .await
        .expect("upsert must be implemented");
    assert_eq!(n1, 1, "insert affects one row");
    assert_eq!(
        svc.get("conf_upsert", "w1").await.unwrap().data["name"],
        serde_json::json!("a")
    );

    // Second upsert on the same PK -> conflict -> UPDATE (not a duplicate row).
    let n2 = svc
        .upsert(
            "conf_upsert",
            UpsertSpec {
                data: vec![
                    ("id".into(), serde_json::json!("w1")),
                    ("name".into(), serde_json::json!("b")),
                ],
                conflict_columns: vec!["id".into()],
                on_conflict: UpsertConflict::SetColumns(vec!["name".into()]),
            },
        )
        .await
        .expect("conflict upsert");
    assert_eq!(n2, 1, "conflict update affects one row");
    assert_eq!(
        svc.get("conf_upsert", "w1").await.unwrap().data["name"],
        serde_json::json!("b"),
        "on-conflict updated a -> b"
    );
    assert_eq!(
        svc.count("conf_upsert", &[]).await.unwrap(),
        1,
        "still one row — updated, not inserted"
    );
}

// ---------------------------------------------------------------------------
// upsert — WindowedCounter (the rate-limiter path that fails open on drift)
// ---------------------------------------------------------------------------

async fn check_upsert_windowed_counter(svc: &dyn DatabaseService) {
    // `key` is the UNIQUE conflict target; the counter/window/timestamp
    // columns model a sliding-window rate limiter.
    let mut key_col = Column::new("key", DataType::Text);
    key_col.nullable = true;
    key_col.unique = true;
    let table = Table {
        name: "conf_rl".to_string(),
        columns: vec![
            pk("id"),
            key_col,
            Column::new("count", DataType::Int).null(),
            Column::new("window_start", DataType::Int64).null(),
            Column::new("created_at", DataType::Text).null(),
            Column::new("updated_at", DataType::Text).null(),
        ],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    };
    reset(svc, &table).await;

    // Seed one counter row with SENTINEL timestamps via a literal INSERT, so
    // "created_at is immutable across conflict-updates" is provable against a
    // fixed value (a re-stamp would overwrite the sentinel). Literal SQL keeps
    // this portable across the `?`/`$1` placeholder dialects.
    let now: i64 = 1_700_000_000;
    let window_cutoff = now - 60; // stored window_start (= now) is NOT expired
    svc.exec_raw(
        &format!(
            "INSERT INTO conf_rl (id, key, count, window_start, created_at, updated_at) \
             VALUES ('seed', 'user:1', 1, {now}, 'SENTINEL-CREATED', 'SENTINEL-UPDATED')"
        ),
        &[],
    )
    .await
    .expect("seed rate-limit row");

    let make_spec = || UpsertSpec {
        data: vec![
            ("id".into(), serde_json::json!("fresh-id")),
            ("key".into(), serde_json::json!("user:1")),
        ],
        conflict_columns: vec!["key".into()],
        on_conflict: UpsertConflict::WindowedCounter {
            count_field: "count".into(),
            window_field: "window_start".into(),
            now,
            window_cutoff,
            created_fields: vec!["created_at".into()],
            updated_fields: vec!["updated_at".into()],
        },
    };

    // First upsert conflicts on `key` -> in-window increment (1 -> 2).
    let n1 = svc
        .upsert("conf_rl", make_spec())
        .await
        .expect("windowed upsert");
    assert_eq!(n1, 1, "conflict update affects the one matching row");
    let r1 = svc.get("conf_rl", "seed").await.expect("get seed");
    assert_eq!(
        field_i64(&r1, "count"),
        2,
        "count incremented 1 -> 2 in window"
    );
    assert_eq!(
        r1.data["created_at"],
        serde_json::json!("SENTINEL-CREATED"),
        "created_at must be immutable on conflict"
    );
    assert_ne!(
        r1.data["updated_at"],
        serde_json::json!("SENTINEL-UPDATED"),
        "updated_at must be re-stamped on conflict"
    );

    // Second in-window upsert increments again (2 -> 3); still no duplicate row.
    let n2 = svc
        .upsert("conf_rl", make_spec())
        .await
        .expect("windowed upsert 2");
    assert_eq!(n2, 1);
    assert_eq!(
        field_i64(&svc.get("conf_rl", "seed").await.unwrap(), "count"),
        3,
        "count incremented 2 -> 3 on the second in-window upsert"
    );
    assert_eq!(
        svc.count("conf_rl", &[]).await.unwrap(),
        1,
        "conflicting upserts never inserted a duplicate row"
    );
}

// ---------------------------------------------------------------------------
// aggregate — grouped Count/Sum/Avg/Max, CaseWhenSum, DateBucket
// ---------------------------------------------------------------------------

async fn check_aggregate(svc: &dyn DatabaseService) {
    // Reuses the immutable conf_read fixture (amount mirrors score):
    //   category x: amounts 10, 20   (created 2026-01-15)
    //   category y: amounts  5,  5   (created 2026-01-16)
    //   category z: amount  100      (created 2026-01-16)

    // Grouped Count + Sum + Avg + Max per category, sorted by category.
    let spec = AggregateSpec {
        select_columns: vec!["category".into()],
        aggregates: vec![
            AggregateColumnSpec::Count {
                alias: "cnt".into(),
            },
            AggregateColumnSpec::Sum {
                field: "amount".into(),
                alias: "total".into(),
            },
            AggregateColumnSpec::Avg {
                field: "amount".into(),
                alias: "mean".into(),
            },
            AggregateColumnSpec::Max {
                field: "amount".into(),
                alias: "top".into(),
            },
        ],
        filters: vec![],
        group_by: vec![GroupBySpec::Column("category".into())],
        sort: vec![SortField {
            field: "category".into(),
            desc: false,
        }],
        limit: 0,
    };
    let groups = svc
        .aggregate("conf_read", spec)
        .await
        .expect("aggregate must be implemented");
    assert_eq!(groups.len(), 3, "three category groups");

    let expect = [
        ("x", 2_i64, 30.0_f64, 15.0_f64, 20.0_f64),
        ("y", 2, 10.0, 5.0, 5.0),
        ("z", 1, 100.0, 100.0, 100.0),
    ];
    for (grp, (cat, cnt, total, mean, top)) in groups.iter().zip(expect) {
        assert_eq!(grp.data["category"], serde_json::json!(cat), "group key");
        assert_eq!(field_i64(grp, "cnt"), cnt, "Count for {cat}");
        assert!(
            (field_f64(grp, "total") - total).abs() < 1e-9,
            "Sum for {cat}"
        );
        assert!(
            (field_f64(grp, "mean") - mean).abs() < 1e-9,
            "Avg for {cat}"
        );
        assert!((field_f64(grp, "top") - top).abs() < 1e-9, "Max for {cat}");
    }

    // CaseWhenSum: conditional count of score >= 20 per category.
    let spec = AggregateSpec {
        select_columns: vec!["category".into()],
        aggregates: vec![
            AggregateColumnSpec::Count {
                alias: "cnt".into(),
            },
            AggregateColumnSpec::CaseWhenSum {
                when: vec![FilterTree::Leaf(filt(
                    "score",
                    FilterOp::GreaterEqual,
                    serde_json::json!(20),
                ))],
                alias: "big".into(),
            },
        ],
        filters: vec![],
        group_by: vec![GroupBySpec::Column("category".into())],
        sort: vec![SortField {
            field: "category".into(),
            desc: false,
        }],
        limit: 0,
    };
    let cw = svc
        .aggregate("conf_read", spec)
        .await
        .expect("aggregate case-when");
    // x: 1 of {10,20} >= 20; y: 0 of {5,5}; z: 1 of {100}.
    assert_eq!(field_i64(&cw[0], "big"), 1, "x has one score >= 20");
    assert_eq!(field_i64(&cw[1], "big"), 0, "y has none >= 20");
    assert_eq!(field_i64(&cw[2], "big"), 1, "z has one >= 20");

    // DateBucket: group by day. Assert the bucket count is right; the bucket
    // *value* decoding is dialect-sensitive, so it is checked only when it
    // comes back as a string (always so on SQLite / D1).
    let spec = AggregateSpec {
        select_columns: vec![],
        aggregates: vec![AggregateColumnSpec::Count {
            alias: "cnt".into(),
        }],
        filters: vec![],
        group_by: vec![GroupBySpec::DateBucket {
            field: "created_at".into(),
        }],
        sort: vec![SortField {
            field: "created_at".into(),
            desc: false,
        }],
        limit: 0,
    };
    let buckets = svc
        .aggregate("conf_read", spec)
        .await
        .expect("aggregate date-bucket");
    assert_eq!(buckets.len(), 2, "two day buckets (Jan 15 and Jan 16)");
    let counts: Vec<i64> = buckets.iter().map(|b| field_i64(b, "cnt")).collect();
    assert_eq!(counts, vec![2, 3], "2 rows on the 15th, 3 on the 16th");
    if let Some(day) = buckets[0].data.get("created_at").and_then(|v| v.as_str()) {
        assert!(
            day.starts_with("2026-01-15"),
            "first bucket is the 15th, got {day}"
        );
    }
}

// ---------------------------------------------------------------------------
// query_raw / exec_raw
// ---------------------------------------------------------------------------

async fn check_raw_sql(svc: &dyn DatabaseService) {
    // query_raw reads from the immutable fixture (literal SQL, no placeholders).
    let rows = svc
        .query_raw("SELECT id, score FROM conf_read WHERE category = 'z'", &[])
        .await
        .expect("query_raw");
    assert_eq!(rows.len(), 1, "one category=z row");
    assert_eq!(rows[0].id, "r5");
    assert_eq!(field_i64(&rows[0], "score"), 100);

    // exec_raw mutates and reports the affected-row count.
    let table = crud_table("conf_exec");
    reset(svc, &table).await;
    for id in ["e1", "e2"] {
        svc.create(
            "conf_exec",
            row([
                ("id", serde_json::json!(id)),
                ("score", serde_json::json!(0)),
            ]),
        )
        .await
        .expect("seed conf_exec");
    }
    let affected = svc
        .exec_raw("UPDATE conf_exec SET score = 42 WHERE id = 'e1'", &[])
        .await
        .expect("exec_raw update");
    assert_eq!(affected, 1, "exec_raw returns rows_affected");
    assert_eq!(
        field_i64(&svc.get("conf_exec", "e1").await.unwrap(), "score"),
        42
    );
    assert_eq!(
        field_i64(&svc.get("conf_exec", "e2").await.unwrap(), "score"),
        0,
        "exec_raw touched only the WHERE-matched row"
    );

    let deleted = svc
        .exec_raw("DELETE FROM conf_exec WHERE id = 'e2'", &[])
        .await
        .expect("exec_raw delete");
    assert_eq!(deleted, 1, "exec_raw delete affected-row count");
    assert_eq!(svc.count("conf_exec", &[]).await.unwrap(), 1);
}

// ---------------------------------------------------------------------------
// schema management — ensure_schema_table[s], exists, add_column, drop_table
// ---------------------------------------------------------------------------

async fn check_schema_management(svc: &dyn DatabaseService) {
    // Start from a clean slate for the schema-under-test table.
    svc.schema_drop_table("conf_schema_x")
        .await
        .expect("pre-drop x");
    assert!(
        !svc.schema_table_exists("conf_schema_x")
            .await
            .expect("exists x (absent)"),
        "table must not exist before creation"
    );

    let table_x = crud_table("conf_schema_x");
    svc.ensure_schema_table(&table_x).await.expect("ensure x");
    assert!(
        svc.schema_table_exists("conf_schema_x")
            .await
            .expect("exists x (present)"),
        "table must exist after ensure_schema_table"
    );

    // schema_add_column adds a usable column: a write to it round-trips.
    svc.schema_add_column(
        "conf_schema_x",
        &Column::new("extra", DataType::Text).null(),
    )
    .await
    .expect("add_column extra");
    svc.create(
        "conf_schema_x",
        row([
            ("id", serde_json::json!("sx1")),
            ("extra", serde_json::json!("v")),
        ]),
    )
    .await
    .expect("write to added column");
    assert_eq!(
        svc.get("conf_schema_x", "sx1").await.unwrap().data["extra"],
        serde_json::json!("v"),
        "value stored in the newly added column round-trips"
    );

    // ensure_schema_tables (the plural default) creates several at once.
    svc.schema_drop_table("conf_schema_a")
        .await
        .expect("pre-drop a");
    svc.schema_drop_table("conf_schema_b")
        .await
        .expect("pre-drop b");
    svc.ensure_schema_tables(&[crud_table("conf_schema_a"), crud_table("conf_schema_b")])
        .await
        .expect("ensure_schema_tables");
    assert!(
        svc.schema_table_exists("conf_schema_a").await.unwrap(),
        "a created"
    );
    assert!(
        svc.schema_table_exists("conf_schema_b").await.unwrap(),
        "b created"
    );

    // schema_drop_table removes a table.
    svc.schema_drop_table("conf_schema_x")
        .await
        .expect("drop x");
    assert!(
        !svc.schema_table_exists("conf_schema_x")
            .await
            .expect("exists x (dropped)"),
        "table must not exist after schema_drop_table"
    );

    // Housekeeping: drop the remaining schema-test tables.
    svc.schema_drop_table("conf_schema_a")
        .await
        .expect("drop a");
    svc.schema_drop_table("conf_schema_b")
        .await
        .expect("drop b");
}
