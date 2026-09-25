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
//! All four are fixed, in the shared renderer / decoder layer and in how the
//! Postgres backend binds parameters; this suite exercises the shapes that
//! tripped the first three, and the Postgres test the fourth (its stored form
//! reads back differently per backend, so it is not a shared check):
//!
//! 2. **`sum` over an `INT` column (FIXED).** The top-level `sum` op decodes its
//!    scalar as `f64`, but Postgres `COALESCE(SUM(int), 0)` returned `INT8`,
//!    which the `f64` decode rejected. The sum is now cast to
//!    `DOUBLE PRECISION` in the statement.
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
//! 1. **Timestamp string vs `TIMESTAMPTZ` (FIXED).** The shared
//!    `create`/`update` path (`stamp_timestamps` in `exec.rs`) auto-stamps
//!    `created_at`/`updated_at` as an RFC3339 *string*, which a real Postgres
//!    `TIMESTAMPTZ` column (declared via `wafer_schema::timestamps()`) refused
//!    as a bound text parameter. The Postgres backend now binds every value by
//!    the type Postgres infers for its parameter, so the string binds as a
//!    timestamp there and as text for a TEXT column — the convention every
//!    block follows, so `expires_at < cutoff` string comparisons keep working.
//!
//! [`exec_raw`]: DatabaseService::exec_raw

use std::collections::HashMap;

use wafer_block::db::{
    ColumnCompareOp, ColumnFilter, Filter, FilterOp, FilterTree, ListOptions, SortField,
};
use wafer_sql_utils::aggregate::CastType;

use super::service::{
    pk, pk_int, AggregateColumnSpec, AggregateSpec, CapGuard, Column, DataType, DatabaseError,
    DatabaseService, GroupBySpec, GuardedInsert, GuardedUpdate, Record, Table, UpsertConflict,
    UpsertSpec, WriteOp, WriteOutcome,
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
/// `set_strict_schema`); `create`/`get` (a taken id refused, the row
/// untouched) and `schema_columns`; a table that numbers its own rows
/// ([`pk_int`]) filling the id of every create path; `count`/`sum` across the full
/// [`FilterOp`] surface; `list` (filter, sort, limit, offset, projection,
/// OR-group `filter_tree`, `total_count`, and pages over a tied sort key
/// ordered by the primary key — single-column, composite, or none);
/// `update`/`update_where`/
/// `update_where_count`; `delete`/`delete_where`/`delete_where_count`/
/// `take_where`; `create_many` (a hundred sparse rows land; a failing row
/// lands none) and `batch` (mixed ops across collections apply in order and
/// report per-op outcomes; one failing op rolls every op back; a filtered
/// delete and the creates that replace the rows it removed are one
/// transaction);
/// `insert_guarded`/`update_guarded` (count and sum caps, landing exactly on
/// a sum cap, the refusing guard named, a replaced row excluded by a filter,
/// no match told apart from a refusal, a taken key as `AlreadyExists`, and ten
/// concurrent inserts under a cap of three leaving exactly three);
/// `increment_field_where` (atomic CAS bump + decrement);
/// `upsert` (`SetColumns` insert-then-update and the rate-limiter
/// `WindowedCounter`, its RFC 3339 stamps and the columns it honours);
/// `aggregate` (grouped `Count`/`Sum`/`Avg`/`Max`,
/// `CaseWhenSum`, and `DateBucket`, then — over `BIGINT` money columns — the
/// `cast_as` output cast, `SumWhere`, and column-to-column predicates in both
/// an aggregate `when` and a `list` filter); `query_raw`/`exec_raw`; and the
/// declared-type value round trip that every backend's row decoder must agree
/// on (see [`check_json_value_round_trip`]); values — `NULL` included — bound
/// by the type of the column they are written to (see
/// [`check_typed_values_round_trip`]); and that a name is never rewritten
/// and a read never adds a column (see
/// [`check_names_are_verbatim_and_reads_never_reshape`]).
pub async fn run_conformance(svc: &dyn DatabaseService) {
    // Exercise the (sync, default-no-op) strict-schema toggle and pin the
    // service into non-strict mode so the suite's explicit schemas drive the
    // behavior. Strict-mode *semantics* are backend-specific (a no-op on
    // adapters that don't cache a schema), so they aren't asserted here.
    svc.set_strict_schema(false);

    check_schema_management(svc).await;
    check_create_get(svc).await;
    check_generated_ids(svc).await;
    check_count_and_sum(svc).await;
    check_list(svc).await;
    check_list_tiebreak(svc).await;
    check_update_family(svc).await;
    check_delete_family(svc).await;
    check_take_where(svc).await;
    check_create_many(svc).await;
    check_batch(svc).await;
    check_batch_delete_where(svc).await;
    check_guarded_writes(svc).await;
    check_increment(svc).await;
    check_upsert_set_columns(svc).await;
    check_upsert_windowed_counter(svc).await;
    check_upsert_windowed_counter_honours_its_columns(svc).await;
    check_aggregate(svc).await;
    check_aggregate_money(svc).await;
    check_raw_sql(svc).await;
    check_json_value_round_trip(svc).await;
    check_typed_values_round_trip(svc).await;
    check_names_are_verbatim_and_reads_never_reshape(svc).await;
    check_names_longer_than_postgres_keeps_are_refused(svc).await;
}

/// Drive two services over **one** database and assert that neither keeps
/// an answer the other has made stale. Panics on the first divergence.
///
/// `a` and `b` must be two independent service instances (two processes'
/// worth of state: separate connections, separate schema caches) that reach
/// the same database — two services opened on one SQLite file, two pools on
/// one Postgres database, two D1 bindings to one database. The suite covers
/// what a multi-replica deployment, or a migration run by another process,
/// relies on: a table `a` saw missing that `b` then creates is visible to
/// `a`'s next read, without `a` having done anything to its own cache.
///
/// Both services are pinned to non-strict mode, the mode in which a read
/// probes the table before running.
pub async fn run_two_instance_conformance(a: &dyn DatabaseService, b: &dyn DatabaseService) {
    a.set_strict_schema(false);
    b.set_strict_schema(false);
    check_table_created_by_another_instance_is_seen(a, b).await;
}

/// `a` reads a table while it is missing, `b` creates it and inserts a row,
/// and every guarded read on `a` then sees the row. A backend that memoized
/// "missing" would answer each of them empty (or zero) until something in
/// `a`'s own process invalidated its cache.
async fn check_table_created_by_another_instance_is_seen(
    a: &dyn DatabaseService,
    b: &dyn DatabaseService,
) {
    let table = crud_table("conf_shared_late");
    b.schema_drop_table(&table.name)
        .await
        .expect("schema_drop_table (idempotent) must succeed");

    let before = a
        .list(&table.name, &ListOptions::default())
        .await
        .expect("list a missing table");
    assert!(
        before.records.is_empty(),
        "the table is missing: {before:?}"
    );
    assert_eq!(a.count(&table.name, &[]).await.expect("count"), 0);

    b.ensure_schema_table(&table)
        .await
        .expect("the other instance creates the table");
    let created = b
        .create(
            &table.name,
            row([
                ("id", serde_json::json!("late-1")),
                ("name", serde_json::json!("seen")),
                ("score", serde_json::json!(7)),
            ]),
        )
        .await
        .expect("the other instance inserts a row");

    let after = a
        .list(&table.name, &ListOptions::default())
        .await
        .expect("list after the other instance created the table");
    assert_eq!(
        after
            .records
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        [created.id.as_str()],
        "a table another instance created must be visible to list"
    );
    assert_eq!(
        a.count(&table.name, &[]).await.expect("count"),
        1,
        "and to count"
    );
    assert!(
        (a.sum(&table.name, "score", &[]).await.expect("sum") - 7.0).abs() < f64::EPSILON,
        "and to sum"
    );

    b.schema_drop_table(&table.name)
        .await
        .expect("drop the shared table");
}

// ---------------------------------------------------------------------------
// Tables that number their own rows
// ---------------------------------------------------------------------------

/// A table whose `id` the database fills ([`pk_int`]: `INTEGER PRIMARY KEY
/// AUTOINCREMENT` on SQLite, `SERIAL` on Postgres) gets the id the database
/// assigned from every create path, and no path mints a string id for it —
/// Postgres would refuse the string, and a minted id would bypass the
/// sequence.
async fn check_generated_ids(svc: &dyn DatabaseService) {
    let table = Table {
        name: "conf_serial".to_string(),
        columns: vec![
            pk_int("id"),
            Column::new("name", DataType::Text).null(),
            Column::new("created_at", DataType::Text).null(),
            Column::new("updated_at", DataType::Text).null(),
        ],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    };
    reset(svc, &table).await;

    let first = svc
        .create("conf_serial", row([("name", serde_json::json!("first"))]))
        .await
        .expect("create without an id in a table that numbers its rows");
    let second = svc
        .create("conf_serial", row([("name", serde_json::json!("second"))]))
        .await
        .expect("second create");
    for (created, name) in [(&first, "first"), (&second, "second")] {
        let id = created.data["id"]
            .as_i64()
            .unwrap_or_else(|| panic!("the returned id is the integer assigned: {created:?}"));
        assert_eq!(created.id, id.to_string(), "{created:?}");
        let got = svc
            .get("conf_serial", &created.id)
            .await
            .expect("get by the returned id");
        assert_eq!(got.data["name"], serde_json::json!(name));
        assert_eq!(got.data["id"], serde_json::json!(id));
    }
    assert_ne!(first.id, second.id, "each row gets its own id");
    // The id string reaches the row on every by-id op.
    let renamed = svc
        .update(
            "conf_serial",
            &first.id,
            row([("name", serde_json::json!("renamed"))]),
        )
        .await
        .expect("update by the returned id");
    assert_eq!(renamed.data["name"], serde_json::json!("renamed"));
    svc.delete("conf_serial", &second.id)
        .await
        .expect("delete by the returned id");

    assert_eq!(
        svc.create_many(
            "conf_serial",
            vec![
                row([("name", serde_json::json!("many-1"))]),
                row([("name", serde_json::json!("many-2"))]),
            ],
        )
        .await
        .expect("create_many without ids"),
        2
    );
    let outcomes = svc
        .batch(vec![WriteOp::Create {
            collection: "conf_serial".into(),
            data: row([("name", serde_json::json!("batched"))]),
        }])
        .await
        .expect("batch create without an id");
    match outcomes.as_slice() {
        [WriteOutcome::Created(r)] => {
            assert!(r.data["id"].is_i64(), "the stored row's id: {r:?}");
        }
        other => panic!("expected one Created, got {other:?}"),
    }
    match svc
        .insert_guarded(
            "conf_serial",
            row([("name", serde_json::json!("guarded"))]),
            &[],
        )
        .await
        .expect("guarded insert without an id")
    {
        GuardedInsert::Inserted(r) => assert!(r.data["id"].is_i64(), "{r:?}"),
        other => panic!("expected Inserted, got {other:?}"),
    }

    let all = svc
        .list("conf_serial", &ListOptions::default())
        .await
        .expect("list");
    let mut ids: Vec<i64> = all
        .records
        .iter()
        .map(|r| {
            r.data["id"]
                .as_i64()
                .unwrap_or_else(|| panic!("an integer id: {r:?}"))
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(
        ids.len(),
        5,
        "five rows, five distinct ids: {:?}",
        all.records
    );
}

// ---------------------------------------------------------------------------
// JSON round trip
// ---------------------------------------------------------------------------

/// A value reads back as the value written: a JSON column holds the JSON
/// value, whatever its kind, and any other column's text is text, however it
/// looks.
///
/// SQL backends in the SQLite family have no array/object storage class, so
/// a JSON column (declared `JSON TEXT`) holds the JSON text of its value and
/// the read path parses it; Postgres stores it in a native `JSONB` column.
/// Every backend must present the same value to block code. Deciding by
/// content instead would hand a user who titled something `[1]` or `{}` an
/// array or an object where they wrote a string, and storing a string in a
/// JSON column unquoted would read `"123"` back as a number.
///
/// Shapes checked:
///
/// - a column **declared TEXT** holding text that is valid JSON — an array, an
///   empty object, an already-serialized object — reads back as that text;
/// - a **lazily added** column first written with a string that looks like
///   JSON (it gets a text type) reads back as the string;
/// - a column **declared JSON** holding a *string* — `"123"`, `"true"`,
///   `"null"`, `"[1]"`, `"{}"`, a serialized object, a word — reads back as
///   that string;
/// - a column declared JSON holding an object, a number (up to `u64::MAX`,
///   which SQLite's NUMERIC affinity would round), a boolean or `null`, and
///   a **lazily added** one first written with an object or array (`JSON
///   TEXT` on SQLite, `JSONB` on Postgres), read back structured;
/// - an id that looks like JSON is still the record's id;
/// - `get`, `list` and `update`'s re-read decode alike, and `update` writes a
///   JSON column the way `create` does.
async fn check_json_value_round_trip(svc: &dyn DatabaseService) {
    let json_columns = [
        "declared_json",
        "serialized_json",
        "word_json",
        "str_123",
        "str_true",
        "str_null",
        "str_array",
        "str_object",
        "num_json",
        "big_json",
        "bool_json",
        "null_json",
    ];
    let mut columns = vec![
        pk("id"),
        Column::new("declared_text", DataType::Text).null(),
        Column::new("empty_object_text", DataType::Text).null(),
        Column::new("serialized_text", DataType::Text).null(),
    ];
    columns.extend(
        json_columns
            .iter()
            .map(|name| Column::new(*name, DataType::Json).null()),
    );
    // `lazy_*` and `plain` are absent so the lazy column-add picks their
    // types from the first value written.
    let table = Table {
        name: "conf_json".to_string(),
        columns,
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    };
    reset(svc, &table).await;

    let object = serde_json::json!({ "k": [1, 2], "nested": { "b": true } });
    let array = serde_json::json!(["a", "b"]);
    let text = |s: &str| serde_json::Value::String(s.to_string());
    let values = vec![
        ("declared_text", text("[1]")),
        ("empty_object_text", text("{}")),
        ("serialized_text", text(&object.to_string())),
        ("lazy_text", text("{\"a\":1}")),
        ("declared_json", object.clone()),
        ("serialized_json", text(&object.to_string())),
        ("word_json", text("not json")),
        ("str_123", text("123")),
        ("str_true", text("true")),
        ("str_null", text("null")),
        ("str_array", text("[1]")),
        ("str_object", text("{}")),
        ("num_json", serde_json::json!(123)),
        ("big_json", serde_json::json!(u64::MAX)),
        ("bool_json", serde_json::json!(true)),
        ("null_json", serde_json::Value::Null),
        ("lazy_json", object.clone()),
        ("lazy_array", array.clone()),
        ("plain", text("not json")),
    ];
    let id = "[1]";
    let mut created = row([("id", serde_json::json!(id))]);
    created.extend(values.iter().map(|(k, v)| ((*k).to_string(), v.clone())));
    svc.create("conf_json", created)
        .await
        .expect("create with JSON-looking payloads must succeed");

    let expect = |rec: &Record, how: &str| {
        for (column, want) in &values {
            assert_eq!(
                rec.data.get(*column),
                Some(want),
                "{how}: column {column:?} must read back as {want:?} (got {:?})",
                rec.data.get(*column)
            );
        }
        assert_eq!(rec.id, id, "{how}: a JSON-looking id is still the id");
    };

    let got = svc.get("conf_json", id).await.expect("get must succeed");
    expect(&got, "get");

    // `list` decodes rows through the same path as `get`; a backend that fixed
    // only one of them would still be inconsistent.
    let listed = svc
        .list(
            "conf_json",
            &ListOptions {
                filters: vec![eq("id", serde_json::json!(id))],
                ..Default::default()
            },
        )
        .await
        .expect("list must succeed");
    assert_eq!(listed.records.len(), 1);
    expect(&listed.records[0], "list");

    // `update` writes JSON columns as `create` does and returns the row
    // re-read after the write.
    let rewritten: HashMap<String, serde_json::Value> = values
        .iter()
        .filter(|(column, _)| json_columns.contains(column))
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect();
    let updated = svc
        .update("conf_json", id, rewritten)
        .await
        .expect("update must succeed");
    expect(&updated, "update");
}

/// A value binds by the type of the column it is written to, not by its own
/// JSON type.
///
/// Postgres fixes each parameter's type when a statement is prepared, and a
/// backend that bound by the value's type failed two ways: a `null` bound as
/// text cannot be written to an `INTEGER`, `BIGINT`, `BOOLEAN` or `JSONB`
/// column at all, and once one execution of an `INSERT` had bound a float for
/// a `DOUBLE PRECISION` column, a later execution of the same SQL carrying an
/// integer had its bytes read as a float (`2` stored as `1e-323`). SQLite
/// accepts both, so the checks pin that every backend does.
async fn check_typed_values_round_trip(svc: &dyn DatabaseService) {
    let table = Table {
        name: "conf_typed".to_string(),
        columns: vec![
            pk("id"),
            Column::new("n", DataType::Int).null(),
            Column::new("big", DataType::Int64).null(),
            Column::new("flag", DataType::Bool).null(),
            Column::new("doc", DataType::Json).null(),
            Column::new("amount", DataType::Float).null(),
        ],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    };
    reset(svc, &table).await;

    let filled = |id: &str, amount: serde_json::Value| {
        row([
            ("id", serde_json::json!(id.to_string())),
            ("n", serde_json::json!(1)),
            ("big", serde_json::json!(2)),
            ("flag", serde_json::json!(true)),
            ("doc", serde_json::json!({ "a": 1 })),
            ("amount", amount),
        ])
    };
    let nulls = || {
        row([
            ("n", serde_json::Value::Null),
            ("big", serde_json::Value::Null),
            ("flag", serde_json::Value::Null),
            ("doc", serde_json::Value::Null),
        ])
    };
    let assert_nulls = |rec: &Record, how: &str| {
        for column in ["n", "big", "flag", "doc"] {
            assert_eq!(
                rec.data.get(column),
                Some(&serde_json::Value::Null),
                "{how}: {column} must read back NULL (got {:?})",
                rec.data.get(column)
            );
        }
    };

    // The same INSERT twice: a float amount, then an integral one.
    svc.create("conf_typed", filled("t1", serde_json::json!(1.5)))
        .await
        .expect("create with a float amount");
    svc.create("conf_typed", filled("t2", serde_json::json!(2)))
        .await
        .expect("create with an integral amount");
    for (id, amount) in [("t1", 1.5), ("t2", 2.0)] {
        let got = svc.get("conf_typed", id).await.expect("get typed row");
        assert!(
            (field_f64(&got, "amount") - amount).abs() < f64::EPSILON,
            "{id}: amount must read back {amount} (got {:?})",
            got.data.get("amount")
        );
        assert_eq!(field_i64(&got, "n"), 1);
        assert_eq!(field_i64(&got, "big"), 2);
        assert_eq!(got.data.get("doc"), Some(&serde_json::json!({ "a": 1 })));
    }

    // NULL into every typed column, through create and through update.
    let mut created = nulls();
    created.insert("id".to_string(), serde_json::json!("t3"));
    svc.create("conf_typed", created)
        .await
        .expect("create with NULLs in typed columns");
    let got = svc.get("conf_typed", "t3").await.expect("get t3");
    assert_nulls(&got, "create");

    let updated = svc
        .update("conf_typed", "t1", nulls())
        .await
        .expect("update typed columns to NULL");
    assert_nulls(&updated, "update");
    assert_nulls(
        &svc.get("conf_typed", "t1").await.expect("get t1"),
        "update, re-read",
    );

    // The null-typed UPDATE again with values: its parameters take the
    // columns' types, not the NULLs' first binding.
    let mut refilled = filled("t1", serde_json::json!(3));
    refilled.remove("id");
    refilled.remove("amount");
    let updated = svc
        .update("conf_typed", "t1", refilled)
        .await
        .expect("update typed columns back to values");
    assert_eq!(field_i64(&updated, "n"), 1);
    assert_eq!(field_i64(&updated, "big"), 2);
    assert_eq!(
        updated.data.get("doc"),
        Some(&serde_json::json!({ "a": 1 }))
    );
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

    // A create naming an id that is already taken fails as AlreadyExists and
    // leaves the stored row exactly as it was — an insert never overwrites.
    let before = svc.get("conf_read", "r1").await.expect("get r1");
    let taken = svc
        .create(
            "conf_read",
            row([
                ("id", serde_json::json!("r1")),
                ("name", serde_json::json!("overwritten")),
                ("score", serde_json::json!(999)),
            ]),
        )
        .await;
    assert!(
        matches!(taken, Err(DatabaseError::AlreadyExists(_))),
        "a create with a taken id must fail as AlreadyExists: {taken:?}"
    );
    let after = svc.get("conf_read", "r1").await.expect("get r1 again");
    assert_eq!(
        after.data, before.data,
        "a refused create must not touch the row"
    );

    // `schema_columns` lists the table's columns, lowercased; a missing table
    // has none.
    let columns = svc
        .schema_columns("conf_read")
        .await
        .expect("schema_columns");
    for column in ["id", "name", "score", "created_at", "updated_at"] {
        assert!(
            columns.iter().any(|c| c == column),
            "schema_columns must list `{column}`: {columns:?}"
        );
    }
    assert!(svc
        .schema_columns("conf_no_such_table")
        .await
        .expect("schema_columns of a missing table")
        .is_empty());

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

    // count, sum and aggregate on a missing table fail safe to zero rows, not
    // an error.
    assert_eq!(
        svc.count("conf_absent_table", &[])
            .await
            .expect("count missing table"),
        0,
        "count on a non-existent table returns 0"
    );
    assert!(
        svc.sum("conf_absent_table", "score", &[])
            .await
            .expect("sum missing table")
            .abs()
            < f64::EPSILON,
        "sum on a non-existent table returns 0"
    );
    let groups = svc
        .aggregate(
            "conf_absent_table",
            AggregateSpec {
                select_columns: vec!["category".into()],
                aggregates: vec![AggregateColumnSpec::Count {
                    alias: "cnt".into(),
                }],
                filters: vec![],
                group_by: vec![GroupBySpec::Column("category".into())],
                sort: vec![],
                limit: 0,
            },
        )
        .await
        .expect("aggregate missing table");
    assert!(
        groups.is_empty(),
        "aggregate on a non-existent table has no groups: {groups:?}"
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
        limit: Some(10),
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
                limit: Some(2),
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
                limit: Some(2),
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

    // No limit returns every row; a zero limit, or an offset with no limit
    // (which SQLite/D1 cannot render), is refused on every backend.
    let everything = svc
        .list("conf_read", &ListOptions::default())
        .await
        .expect("list with no limit");
    assert_eq!(everything.records.len(), 5, "no limit returns every row");
    for (limit, offset, what) in [
        (Some(0), 0, "a zero limit"),
        (None, 1, "an offset with no limit"),
    ] {
        let err = svc
            .list(
                "conf_read",
                &ListOptions {
                    limit,
                    offset,
                    ..Default::default()
                },
            )
            .await
            .expect_err(what);
        assert!(
            matches!(err, DatabaseError::InvalidArgument(_)),
            "{what} must be InvalidArgument, got {err:?}"
        );
    }

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
// list — ties on the sort key break on the primary key
// ---------------------------------------------------------------------------

/// Page through `table` two rows at a time, sorted by `sort`, and return each
/// page's `key` values (a composite key's columns joined with `/`).
async fn pages(
    svc: &dyn DatabaseService,
    table: &str,
    sort: &[(&str, bool)],
    key: &[&str],
) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for offset in [0, 2, 4] {
        let listed = svc
            .list(
                table,
                &ListOptions {
                    sort: sort
                        .iter()
                        .map(|(field, desc)| SortField {
                            field: (*field).into(),
                            desc: *desc,
                        })
                        .collect(),
                    limit: Some(2),
                    offset,
                    ..Default::default()
                },
            )
            .await
            .unwrap_or_else(|e| panic!("list {table} offset {offset}: {e:?}"));
        assert_eq!(listed.total_count, 5, "{table}: total_count");
        out.push(
            listed
                .records
                .iter()
                .map(|r| {
                    key.iter()
                        .map(|k| r.data[*k].as_str().expect("text key").to_string())
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .collect(),
        );
    }
    out
}

/// Five rows share one sort-key value and are inserted out of key order.
/// Paging through them must return every row exactly once, in primary-key
/// order: the key breaks the tie, in the direction of the last sort term.
/// Without it a backend returns ties in storage order — insertion order on
/// SQLite — so the pages here would read `c a | e b | d`.
///
/// Covered for a table keyed by `id`, a table whose `id` the backend mints
/// (ties list in creation order), a table keyed by another column (no `id`
/// at all), a composite key, and a table with no primary key, whose select
/// must still be valid SQL.
async fn check_list_tiebreak(svc: &dyn DatabaseService) {
    let tied = |table: &str, key_cols: Vec<Column>, primary_key: Vec<String>| Table {
        name: table.to_string(),
        columns: key_cols
            .into_iter()
            .chain([Column::new("created_at", DataType::Text)])
            .collect(),
        indexes: Vec::new(),
        primary_key,
        unique_keys: Vec::new(),
    };
    let order = ["c", "a", "e", "b", "d"];

    // Keyed by `id`.
    reset(svc, &tied("conf_tie_id", vec![pk("id")], Vec::new())).await;
    for id in order {
        svc.create(
            "conf_tie_id",
            row([
                ("id", serde_json::json!(id)),
                ("created_at", serde_json::json!("2026-01-01T00:00:00Z")),
            ]),
        )
        .await
        .expect("seed conf_tie_id");
    }
    assert_eq!(
        pages(svc, "conf_tie_id", &[("created_at", true)], &["id"]).await,
        vec![vec!["e", "d"], vec!["c", "b"], vec!["a"]],
        "newest-first ties break id-descending"
    );
    assert_eq!(
        pages(svc, "conf_tie_id", &[("created_at", false)], &["id"]).await,
        vec![vec!["a", "b"], vec!["c", "d"], vec!["e"]],
        "oldest-first ties break id-ascending"
    );
    assert_eq!(
        pages(svc, "conf_tie_id", &[], &["id"]).await,
        vec![vec!["a", "b"], vec!["c", "d"], vec!["e"]],
        "a paged list with no sort is in key order"
    );

    // Keyed by an `id` the backend mints: rows created in order without an
    // id come back in creation order when their sort key ties, because the
    // minted id is time-ordered. A random id would shuffle them.
    reset(
        svc,
        &tied(
            "conf_tie_minted",
            vec![pk("id"), Column::new("label", DataType::Text)],
            Vec::new(),
        ),
    )
    .await;
    for label in order {
        svc.create(
            "conf_tie_minted",
            row([
                ("label", serde_json::json!(label)),
                ("created_at", serde_json::json!("2026-01-01T00:00:00Z")),
            ]),
        )
        .await
        .expect("seed conf_tie_minted");
    }
    assert_eq!(
        pages(svc, "conf_tie_minted", &[("created_at", false)], &["label"])
            .await
            .concat(),
        order,
        "oldest-first ties on a minted id list in creation order"
    );
    let mut newest_first = order;
    newest_first.reverse();
    assert_eq!(
        pages(svc, "conf_tie_minted", &[("created_at", true)], &["label"])
            .await
            .concat(),
        newest_first,
        "newest-first ties on a minted id list in reverse creation order"
    );

    // Keyed by a column other than `id`; the upsert path writes rows without
    // synthesizing an `id` column.
    reset(
        svc,
        &tied("conf_tie_hash", vec![pk("token_hash")], Vec::new()),
    )
    .await;
    for hash in order {
        svc.upsert(
            "conf_tie_hash",
            UpsertSpec {
                data: vec![
                    ("token_hash".into(), serde_json::json!(hash)),
                    (
                        "created_at".into(),
                        serde_json::json!("2026-01-01T00:00:00Z"),
                    ),
                ],
                conflict_columns: vec!["token_hash".into()],
                on_conflict: UpsertConflict::SetColumns(vec!["created_at".into()]),
            },
        )
        .await
        .expect("seed conf_tie_hash");
    }
    assert_eq!(
        pages(
            svc,
            "conf_tie_hash",
            &[("created_at", true)],
            &["token_hash"]
        )
        .await,
        vec![vec!["e", "d"], vec!["c", "b"], vec!["a"]],
        "a non-`id` key breaks the tie"
    );

    // A composite key, declared (user, role) though the columns are listed
    // role-first: the tiebreak follows key order, not column order.
    reset(
        svc,
        &tied(
            "conf_tie_pair",
            vec![
                Column::new("role_id", DataType::Text),
                Column::new("user_id", DataType::Text),
            ],
            vec!["user_id".into(), "role_id".into()],
        ),
    )
    .await;
    for (user, role) in [
        ("u2", "r1"),
        ("u1", "r2"),
        ("u3", "r1"),
        ("u1", "r1"),
        ("u2", "r2"),
    ] {
        svc.upsert(
            "conf_tie_pair",
            UpsertSpec {
                data: vec![
                    ("user_id".into(), serde_json::json!(user)),
                    ("role_id".into(), serde_json::json!(role)),
                    (
                        "created_at".into(),
                        serde_json::json!("2026-01-01T00:00:00Z"),
                    ),
                ],
                conflict_columns: vec!["user_id".into(), "role_id".into()],
                on_conflict: UpsertConflict::SetColumns(vec!["created_at".into()]),
            },
        )
        .await
        .expect("seed conf_tie_pair");
    }
    assert_eq!(
        pages(
            svc,
            "conf_tie_pair",
            &[("created_at", false)],
            &["user_id", "role_id"]
        )
        .await,
        vec![
            vec!["u1/r1", "u1/r2"],
            vec!["u2/r1", "u2/r2"],
            vec!["u3/r1"]
        ],
        "a composite key breaks the tie column by column, in key order"
    );

    // No primary key: nothing to break the tie with, but the paged, sorted
    // select must still run and cover the table.
    reset(
        svc,
        &tied(
            "conf_tie_keyless",
            vec![Column::new("label", DataType::Text)],
            Vec::new(),
        ),
    )
    .await;
    // `create` adds an `id` column here (lazily, not as a key), which is
    // fine: the table still has no primary key.
    for label in order {
        svc.create(
            "conf_tie_keyless",
            row([
                ("label", serde_json::json!(label)),
                ("created_at", serde_json::json!("2026-01-01T00:00:00Z")),
            ]),
        )
        .await
        .expect("seed conf_tie_keyless");
    }
    let mut labels: Vec<String> =
        pages(svc, "conf_tie_keyless", &[("created_at", true)], &["label"])
            .await
            .concat();
    assert_eq!(labels.len(), 5, "every row is on some page: {labels:?}");
    labels.sort();
    labels.dedup();
    assert_eq!(labels.len(), 5, "a keyless table still lists every row");
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
// create_many (all-or-nothing multi-row insert)
// ---------------------------------------------------------------------------

async fn check_create_many(svc: &dyn DatabaseService) {
    let table = crud_table("conf_create_many");
    reset(svc, &table).await;

    // A hundred rows, no ids (each is minted), with differing column sets:
    // every third row carries a `note` the others omit.
    let rows: Vec<_> = (0..100)
        .map(|i| {
            let mut r = row([
                ("name", serde_json::json!(format!("m{i:03}"))),
                ("score", serde_json::json!(i)),
            ]);
            if i % 3 == 0 {
                r.insert("note".into(), serde_json::json!("third"));
            }
            r
        })
        .collect();
    let inserted = svc
        .create_many("conf_create_many", rows)
        .await
        .expect("create_many of 100 rows");
    assert_eq!(inserted, 100, "create_many reports every row inserted");
    assert_eq!(
        svc.count("conf_create_many", &[]).await.expect("count"),
        100,
        "all 100 rows landed"
    );
    assert_eq!(
        svc.count(
            "conf_create_many",
            &[eq("note", serde_json::json!("third"))]
        )
        .await
        .expect("count notes"),
        34,
        "the sparse column landed on exactly the rows that carried it"
    );
    let listed = svc
        .list(
            "conf_create_many",
            &ListOptions {
                filters: vec![eq("name", serde_json::json!("m042"))],
                ..Default::default()
            },
        )
        .await
        .expect("list m042");
    assert_eq!(listed.records.len(), 1);
    let m042 = &listed.records[0];
    assert!(!m042.id.is_empty(), "a minted id");
    assert_eq!(field_i64(m042, "score"), 42);
    assert!(
        m042.data["created_at"].is_string() && m042.data["updated_at"].is_string(),
        "rows are stamped like create: {:?}",
        m042.data
    );

    // One failing row (a duplicate primary key, third of four) lands NONE of
    // the call's rows — the two before it are rolled back.
    svc.create(
        "conf_create_many",
        row([("id", serde_json::json!("taken"))]),
    )
    .await
    .expect("seed the conflicting id");
    let err = svc
        .create_many(
            "conf_create_many",
            vec![
                row([("id", serde_json::json!("fresh1"))]),
                row([("id", serde_json::json!("fresh2"))]),
                row([("id", serde_json::json!("taken"))]),
                row([("id", serde_json::json!("fresh3"))]),
            ],
        )
        .await;
    assert!(
        matches!(err, Err(DatabaseError::AlreadyExists(_))),
        "a duplicate key fails the call as AlreadyExists: {err:?}"
    );
    assert_eq!(
        svc.count("conf_create_many", &[]).await.expect("count"),
        101,
        "no row of the failed call landed"
    );
    for id in ["fresh1", "fresh2", "fresh3"] {
        assert!(
            matches!(
                svc.get("conf_create_many", id).await,
                Err(DatabaseError::NotFound)
            ),
            "{id} must not exist after the failed call"
        );
    }

    assert_eq!(
        svc.create_many("conf_create_many", Vec::new())
            .await
            .expect("empty create_many"),
        0
    );
}

// ---------------------------------------------------------------------------
// batch (all-or-nothing mixed writes across collections)
// ---------------------------------------------------------------------------

async fn check_batch(svc: &dyn DatabaseService) {
    let table = crud_table("conf_batch");
    reset(svc, &table).await;
    let other = crud_table("conf_batch_other");
    reset(svc, &other).await;
    for (id, category) in [("b1", "a"), ("b2", "a"), ("b3", "b")] {
        svc.create(
            "conf_batch",
            row([
                ("id", serde_json::json!(id)),
                ("category", serde_json::json!(category)),
                ("name", serde_json::json!("orig")),
                ("score", serde_json::json!(1)),
            ]),
        )
        .await
        .expect("seed conf_batch");
    }

    let upsert = |id: &str, name: &str| WriteOp::Upsert {
        collection: "conf_batch".into(),
        spec: UpsertSpec {
            data: vec![
                ("id".into(), serde_json::json!(id)),
                ("name".into(), serde_json::json!(name)),
            ],
            conflict_columns: vec!["id".into()],
            on_conflict: UpsertConflict::SetColumns(vec!["name".into()]),
        },
    };
    let outcomes = svc
        .batch(vec![
            WriteOp::Create {
                collection: "conf_batch".into(),
                data: row([
                    ("id", serde_json::json!("b4")),
                    ("name", serde_json::json!("new")),
                ]),
            },
            // Sees the create above: same transaction, in order.
            WriteOp::Update {
                collection: "conf_batch".into(),
                id: "b4".into(),
                data: row([("score", serde_json::json!(7))]),
            },
            WriteOp::Update {
                collection: "conf_batch".into(),
                id: "b1".into(),
                data: row([("score", serde_json::json!(5))]),
            },
            WriteOp::Delete {
                collection: "conf_batch".into(),
                id: "b2".into(),
            },
            WriteOp::UpdateWhere {
                collection: "conf_batch".into(),
                filters: vec![eq("category", serde_json::json!("b"))],
                data: row([("name", serde_json::json!("Y"))]),
            },
            upsert("b5", "upserted"),
            WriteOp::Update {
                collection: "conf_batch".into(),
                id: "missing".into(),
                data: row([("score", serde_json::json!(0))]),
            },
            WriteOp::Delete {
                collection: "conf_batch".into(),
                id: "missing".into(),
            },
            WriteOp::Create {
                collection: "conf_batch_other".into(),
                data: row([("name", serde_json::json!("elsewhere"))]),
            },
        ])
        .await
        .expect("mixed batch");
    assert_eq!(outcomes.len(), 9, "one outcome per op");
    match &outcomes[0] {
        WriteOutcome::Created(r) => {
            assert_eq!(r.id, "b4");
            assert_eq!(r.data["name"], serde_json::json!("new"));
            assert!(r.data["created_at"].is_string(), "stored row: {:?}", r.data);
        }
        other => panic!("op 0: expected Created, got {other:?}"),
    }
    match &outcomes[1] {
        WriteOutcome::Updated(Some(r)) => {
            assert_eq!(r.id, "b4");
            assert_eq!(field_i64(r, "score"), 7, "the updated row is returned");
            assert_eq!(r.data["name"], serde_json::json!("new"));
        }
        other => panic!("op 1: expected Updated(Some), got {other:?}"),
    }
    assert!(
        matches!(&outcomes[2], WriteOutcome::Updated(Some(r)) if field_i64(r, "score") == 5),
        "op 2: {:?}",
        outcomes[2]
    );
    assert!(
        matches!(outcomes[3], WriteOutcome::Deleted { rows_affected: 1 }),
        "op 3: {:?}",
        outcomes[3]
    );
    assert!(
        matches!(outcomes[4], WriteOutcome::UpdatedWhere { rows_affected: 1 }),
        "op 4: {:?}",
        outcomes[4]
    );
    assert!(
        matches!(outcomes[5], WriteOutcome::Upserted { rows_affected: 1 }),
        "op 5: {:?}",
        outcomes[5]
    );
    assert!(
        matches!(outcomes[6], WriteOutcome::Updated(None)),
        "op 6: an update of a missing id is an outcome, not a failure: {:?}",
        outcomes[6]
    );
    assert!(
        matches!(outcomes[7], WriteOutcome::Deleted { rows_affected: 0 }),
        "op 7: a delete of a missing id deletes nothing: {:?}",
        outcomes[7]
    );
    let elsewhere_id = match &outcomes[8] {
        WriteOutcome::Created(r) => {
            assert!(!r.id.is_empty(), "a minted id comes back");
            r.id.clone()
        }
        other => panic!("op 8: expected Created, got {other:?}"),
    };

    // Everything persisted.
    assert_eq!(
        field_i64(&svc.get("conf_batch", "b4").await.expect("b4"), "score"),
        7
    );
    assert_eq!(
        field_i64(&svc.get("conf_batch", "b1").await.expect("b1"), "score"),
        5
    );
    assert!(matches!(
        svc.get("conf_batch", "b2").await,
        Err(DatabaseError::NotFound)
    ));
    assert_eq!(
        svc.get("conf_batch", "b3").await.expect("b3").data["name"],
        serde_json::json!("Y")
    );
    assert_eq!(
        svc.get("conf_batch", "b5").await.expect("b5").data["name"],
        serde_json::json!("upserted")
    );
    assert_eq!(
        svc.get("conf_batch_other", &elsewhere_id)
            .await
            .expect("other collection")
            .data["name"],
        serde_json::json!("elsewhere")
    );

    // One failing op — a create reusing b3's primary key, after writes to
    // both collections — rolls EVERY op of the batch back.
    let before = svc.count("conf_batch", &[]).await.expect("count");
    let err = svc
        .batch(vec![
            WriteOp::Create {
                collection: "conf_batch".into(),
                data: row([("id", serde_json::json!("rolled"))]),
            },
            WriteOp::Update {
                collection: "conf_batch".into(),
                id: "b1".into(),
                data: row([("name", serde_json::json!("rolled"))]),
            },
            WriteOp::Delete {
                collection: "conf_batch".into(),
                id: "b4".into(),
            },
            WriteOp::UpdateWhere {
                collection: "conf_batch_other".into(),
                filters: vec![eq("name", serde_json::json!("elsewhere"))],
                data: row([("name", serde_json::json!("rolled"))]),
            },
            upsert("b5", "rolled"),
            WriteOp::Create {
                collection: "conf_batch".into(),
                data: row([("id", serde_json::json!("b3"))]),
            },
        ])
        .await;
    assert!(err.is_err(), "a duplicate key fails the batch: {err:?}");
    assert_eq!(
        svc.count("conf_batch", &[]).await.expect("count"),
        before,
        "no row added or removed by the failed batch"
    );
    assert!(matches!(
        svc.get("conf_batch", "rolled").await,
        Err(DatabaseError::NotFound)
    ));
    assert_eq!(
        svc.get("conf_batch", "b1").await.expect("b1").data["name"],
        serde_json::json!("orig"),
        "the update was rolled back"
    );
    svc.get("conf_batch", "b4")
        .await
        .expect("the delete was rolled back");
    assert_eq!(
        svc.get("conf_batch", "b5").await.expect("b5").data["name"],
        serde_json::json!("upserted"),
        "the upsert was rolled back"
    );
    assert_eq!(
        svc.get("conf_batch_other", &elsewhere_id)
            .await
            .expect("other collection")
            .data["name"],
        serde_json::json!("elsewhere"),
        "the other collection's update was rolled back too"
    );

    assert!(svc
        .batch(Vec::new())
        .await
        .expect("an empty batch")
        .is_empty());

    // An `UpdateWhere` against a missing table matches nothing, as the single
    // `update_where_count` does: it does not abort the batch around it.
    svc.schema_drop_table("conf_batch_missing")
        .await
        .expect("drop (idempotent)");
    assert_eq!(
        svc.update_where_count(
            "conf_batch_missing",
            &[eq("category", serde_json::json!("a"))],
            row([("name", serde_json::json!("Z"))]),
        )
        .await
        .expect("single op on a missing table"),
        0
    );
    let outcomes = svc
        .batch(vec![
            WriteOp::Create {
                collection: "conf_batch".into(),
                data: row([("id", serde_json::json!("beside_missing"))]),
            },
            WriteOp::UpdateWhere {
                collection: "conf_batch_missing".into(),
                filters: vec![eq("category", serde_json::json!("a"))],
                data: row([("name", serde_json::json!("Z"))]),
            },
        ])
        .await
        .expect("an update-where on a missing table does not fail the batch");
    assert!(
        matches!(outcomes[1], WriteOutcome::UpdatedWhere { rows_affected: 0 }),
        "{:?}",
        outcomes[1]
    );
    svc.get("conf_batch", "beside_missing")
        .await
        .expect("the op beside it committed");
}

/// `batch` with a `DeleteWhere`: the replace-a-table shape — delete the rows
/// a filter matches, then create their replacements — is ONE transaction, so
/// a replacement that fails leaves the original rows in place.
async fn check_batch_delete_where(svc: &dyn DatabaseService) {
    let t = "conf_batch_replace";
    reset(svc, &crud_table(t)).await;
    for (id, category) in [("r1", "a"), ("r2", "a"), ("r3", "b")] {
        svc.create(
            t,
            row([
                ("id", serde_json::json!(id)),
                ("category", serde_json::json!(category)),
                ("name", serde_json::json!("orig")),
            ]),
        )
        .await
        .expect("seed conf_batch_replace");
    }
    let create = |id: &str| WriteOp::Create {
        collection: t.into(),
        data: row([
            ("id", serde_json::json!(id)),
            ("category", serde_json::json!("a")),
            ("name", serde_json::json!("new")),
        ]),
    };
    let ids = |records: Vec<Record>| {
        let mut ids: Vec<String> = records.into_iter().map(|r| r.id).collect();
        ids.sort();
        ids
    };
    let all_ids = || async {
        ids(svc
            .list(t, &ListOptions::default())
            .await
            .expect("list conf_batch_replace")
            .records)
    };

    // A filtered delete reports how many rows it removed, and a create after
    // it may reuse a removed row's id: the delete ran first, in the same
    // transaction.
    let outcomes = svc
        .batch(vec![
            WriteOp::DeleteWhere {
                collection: t.into(),
                filters: vec![eq("category", serde_json::json!("a"))],
            },
            create("r1"),
            create("r4"),
        ])
        .await
        .expect("delete-where then create");
    assert!(
        matches!(outcomes[0], WriteOutcome::DeletedWhere { rows_affected: 2 }),
        "op 0: {:?}",
        outcomes[0]
    );
    assert_eq!(all_ids().await, ["r1", "r3", "r4"]);
    assert_eq!(
        svc.get(t, "r1").await.expect("r1").data["name"],
        serde_json::json!("new"),
        "r1 is the replacement, not the original"
    );
    assert_eq!(
        svc.get(t, "r3").await.expect("r3").data["name"],
        serde_json::json!("orig"),
        "a row the filter did not match stays"
    );

    // Replace the whole table (no filters), with the last replacement
    // failing on a key an earlier one took: nothing is deleted and nothing
    // is created.
    let err = svc
        .batch(vec![
            WriteOp::DeleteWhere {
                collection: t.into(),
                filters: Vec::new(),
            },
            create("r5"),
            create("r6"),
            create("r5"),
        ])
        .await;
    assert!(
        err.is_err(),
        "a duplicate key fails the replacement: {err:?}"
    );
    assert_eq!(
        all_ids().await,
        ["r1", "r3", "r4"],
        "the failed replacement left the original rows in place"
    );

    // A `DeleteWhere` against a missing table matches nothing, as the single
    // `delete_where_count` does: it does not abort the batch around it.
    svc.schema_drop_table("conf_batch_replace_missing")
        .await
        .expect("drop (idempotent)");
    let outcomes = svc
        .batch(vec![
            WriteOp::DeleteWhere {
                collection: "conf_batch_replace_missing".into(),
                filters: Vec::new(),
            },
            create("beside_missing"),
        ])
        .await
        .expect("a delete-where on a missing table does not fail the batch");
    assert!(
        matches!(outcomes[0], WriteOutcome::DeletedWhere { rows_affected: 0 }),
        "{:?}",
        outcomes[0]
    );
    svc.get(t, "beside_missing")
        .await
        .expect("the op beside it committed");
}

// ---------------------------------------------------------------------------
// insert_guarded / update_guarded (the check and the write are one step)
// ---------------------------------------------------------------------------

/// The quota-shaped table the guarded-write checks write to: an owner, a
/// bucket and a byte size per row.
fn guarded_table() -> Table {
    Table {
        name: "conf_guarded".to_string(),
        columns: vec![
            pk("id"),
            Column::new("owner", DataType::Text).null(),
            Column::new("bucket", DataType::Text).null(),
            // BIGINT: `SUM(<bigint>)` is NUMERIC on Postgres, which the guard
            // compares against bound BIGINT caps.
            Column::new("size", DataType::Int64).null(),
            Column::new("created_at", DataType::Text).null(),
            Column::new("updated_at", DataType::Text).null(),
        ],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    }
}

/// At most `cap` rows per `(owner, bucket)`.
fn files_per_bucket(owner: &str, bucket: &str, cap: i64) -> CapGuard {
    CapGuard::CountBelow {
        filters: vec![
            eq("owner", serde_json::json!(owner)),
            eq("bucket", serde_json::json!(bucket)),
        ],
        cap,
    }
}

/// At most `cap` bytes per owner once `add` more land, not counting the rows
/// `except` names (the row an update replaces).
fn bytes_per_owner(owner: &str, except: Option<&str>, add: i64, cap: i64) -> CapGuard {
    let mut filters = vec![eq("owner", serde_json::json!(owner))];
    if let Some(id) = except {
        filters.push(filt("id", FilterOp::NotEqual, serde_json::json!(id)));
    }
    CapGuard::SumAtMost {
        field: "size".into(),
        filters,
        add,
        cap,
    }
}

fn guarded_row(owner: &str, bucket: &str, size: i64) -> HashMap<String, serde_json::Value> {
    row([
        ("owner", serde_json::json!(owner)),
        ("bucket", serde_json::json!(bucket)),
        ("size", serde_json::json!(size)),
    ])
}

/// The row an admitted guarded insert stored; panics on a refusal.
fn inserted(outcome: &GuardedInsert) -> &Record {
    match outcome {
        GuardedInsert::Inserted(record) => record,
        GuardedInsert::Refused { guard } => panic!("expected an insert, guard {guard} refused it"),
    }
}

/// The index of the guard that refused an insert; `None` when it landed.
fn refused_by(outcome: &GuardedInsert) -> Option<usize> {
    match outcome {
        GuardedInsert::Inserted(_) => None,
        GuardedInsert::Refused { guard } => Some(*guard),
    }
}

async fn check_guarded_writes(svc: &dyn DatabaseService) {
    reset(svc, &guarded_table()).await;
    let t = "conf_guarded";

    // CountBelow: the third file in one (owner, bucket) is refused by that
    // guard; another bucket of the same owner is its own count.
    let mut landed = Vec::new();
    for _ in 0..3 {
        landed.push(
            svc.insert_guarded(
                t,
                guarded_row("c", "b1", 1),
                &[files_per_bucket("c", "b1", 2)],
            )
            .await
            .expect("insert_guarded (count)"),
        );
    }
    let verdicts: Vec<Option<usize>> = landed.iter().map(refused_by).collect();
    assert_eq!(
        verdicts,
        [None, None, Some(0)],
        "the third file passes a cap of two"
    );
    let first = inserted(&landed[0]);
    assert!(!first.id.is_empty(), "a minted id");
    assert_eq!(field_i64(first, "size"), 1, "the stored row comes back");
    assert!(
        first.data["created_at"].is_string() && first.data["updated_at"].is_string(),
        "rows are stamped like create: {:?}",
        first.data
    );
    inserted(
        &svc.insert_guarded(
            t,
            guarded_row("c", "b2", 1),
            &[files_per_bucket("c", "b2", 2)],
        )
        .await
        .expect("insert_guarded (other bucket)"),
    );
    assert_eq!(
        svc.count(t, &[eq("owner", serde_json::json!("c"))])
            .await
            .expect("count"),
        3,
        "exactly the admitted rows landed"
    );

    // SumAtMost: landing exactly on the cap is admitted, one byte past it is
    // not.
    for (id, size, verdict) in [("s1", 60, None), ("s2", 40, None), ("s3", 1, Some(0))] {
        let mut data = guarded_row("s", "b1", size);
        data.insert("id".into(), serde_json::json!(id));
        let got = svc
            .insert_guarded(t, data, &[bytes_per_owner("s", None, size, 100)])
            .await
            .expect("insert_guarded (sum)");
        assert_eq!(refused_by(&got), verdict, "{id} of {size} bytes: {got:?}");
    }
    assert!(
        matches!(svc.get(t, "s3").await, Err(DatabaseError::NotFound)),
        "the refused row is not stored"
    );

    // The refusal names the guard that failed: the byte cap in second place
    // here, the file cap in first place when both fail.
    let two_guards = |files: i64| {
        [
            files_per_bucket("s", "b1", files),
            bytes_per_owner("s", None, 5, 100),
        ]
    };
    for (files, verdict) in [(10, Some(1)), (1, Some(0))] {
        let got = svc
            .insert_guarded(t, guarded_row("s", "b1", 5), &two_guards(files))
            .await
            .expect("insert_guarded (two guards)");
        assert_eq!(refused_by(&got), verdict, "file cap {files}: {got:?}");
    }
    // No guards: an unconditional insert.
    inserted(
        &svc.insert_guarded(t, guarded_row("n", "b1", 1), &[])
            .await
            .expect("insert_guarded (no guards)"),
    );
    // A taken key is AlreadyExists — not a refusal, not an internal fault —
    // whether or not guards are given, and for a plain create too.
    for guards in [Vec::new(), vec![files_per_bucket("x", "b1", 10)]] {
        let mut data = guarded_row("x", "b1", 1);
        data.insert("id".into(), serde_json::json!("s1"));
        let got = svc.insert_guarded(t, data, &guards).await;
        assert!(
            matches!(got, Err(DatabaseError::AlreadyExists(_))),
            "a duplicate id with {} guard(s): {got:?}",
            guards.len()
        );
    }
    let got = svc.create(t, row([("id", serde_json::json!("s1"))])).await;
    assert!(
        matches!(got, Err(DatabaseError::AlreadyExists(_))),
        "create of a duplicate id: {got:?}"
    );

    // update_guarded replacing s1 (60 of the owner's 100 bytes): the sum
    // excludes s1 itself, so 40 + 60 fits exactly and 40 + 61 does not.
    let replace = |id: &str, size: i64| {
        (
            vec![eq("id", serde_json::json!(id))],
            row([("size", serde_json::json!(size))]),
            [bytes_per_owner("s", Some(id), size, 100)],
        )
    };
    let (filters, data, guards) = replace("s1", 61);
    let got = svc
        .update_guarded(t, &filters, data, &guards)
        .await
        .expect("update_guarded (refused)");
    assert!(
        matches!(got, GuardedUpdate::Refused { guard: 0 }),
        "40 + 61 > 100: {got:?}"
    );
    assert_eq!(field_i64(&svc.get(t, "s1").await.expect("s1"), "size"), 60);
    let (filters, data, guards) = replace("s1", 55);
    let got = svc
        .update_guarded(t, &filters, data, &guards)
        .await
        .expect("update_guarded");
    assert!(
        matches!(got, GuardedUpdate::Updated { rows_affected: 1 }),
        "{got:?}"
    );
    assert_eq!(field_i64(&svc.get(t, "s1").await.expect("s1"), "size"), 55);
    // Every guard holds but no row matches: NoMatch, not a refusal — the
    // row a takeover expected is gone.
    let (filters, data, guards) = replace("gone", 1);
    let got = svc
        .update_guarded(t, &filters, data, &guards)
        .await
        .expect("update_guarded (no match)");
    assert!(matches!(got, GuardedUpdate::NoMatch), "{got:?}");
    let got = svc
        .update_guarded(
            "conf_guarded_missing",
            &[],
            row([("size", serde_json::json!(1))]),
            &[],
        )
        .await
        .expect("update_guarded (missing table)");
    assert!(
        matches!(got, GuardedUpdate::NoMatch),
        "a missing table matches nothing, as update_where_count: {got:?}"
    );

    // Ten concurrent inserts under a cap of three: the check and the insert
    // are one step, so exactly three land. (Every write waits on SQLite's one
    // writer; on PostgreSQL the per-table guard lock is what serialises them
    // — see wafer-block-postgres's conformance test, which forces the race.)
    let cap_of_three = [files_per_bucket("r", "b1", 3)];
    let racers = (0..10).map(|_| svc.insert_guarded(t, guarded_row("r", "b1", 30), &cap_of_three));
    let results = futures::future::join_all(racers).await;
    let admitted = results
        .iter()
        .filter(|r| refused_by(r.as_ref().expect("concurrent insert_guarded")).is_none())
        .count();
    assert_eq!(admitted, 3, "exactly the cap is admitted: {results:?}");
    assert_eq!(
        svc.count(t, &[eq("owner", serde_json::json!("r"))])
            .await
            .expect("count"),
        3
    );
    // The same for a byte cap: ten concurrent 30-byte uploads under 100.
    let hundred_bytes = [bytes_per_owner("q", None, 30, 100)];
    let racers = (0..10).map(|_| svc.insert_guarded(t, guarded_row("q", "b1", 30), &hundred_bytes));
    let admitted = futures::future::join_all(racers)
        .await
        .into_iter()
        .filter(|r| refused_by(r.as_ref().expect("concurrent insert_guarded")).is_none())
        .count();
    assert_eq!(admitted, 3, "30 * 3 <= 100 < 30 * 4");
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

    // A fresh counter row carries one RFC 3339 instant in both timestamp
    // columns, the form `create` stamps, so a retention sweep's
    // `updated_at < <RFC 3339 cutoff>` never takes a row written after the
    // cutoff. SQL `CURRENT_TIMESTAMP` text (`YYYY-MM-DD HH:MM:SS…`) sorts
    // before every same-day RFC 3339 value (`' '` < `'T'`).
    let before = chrono::Utc::now().to_rfc3339();
    let mut fresh = make_spec();
    fresh.data = vec![
        ("id".into(), serde_json::json!("fresh-2")),
        ("key".into(), serde_json::json!("user:2")),
    ];
    svc.upsert("conf_rl", fresh)
        .await
        .expect("windowed upsert inserting a fresh row");
    let inserted = svc.get("conf_rl", "fresh-2").await.expect("get fresh row");
    assert_eq!(field_i64(&inserted, "count"), 1);
    let created = inserted.data["created_at"]
        .as_str()
        .expect("created_at is text");
    assert!(
        chrono::DateTime::parse_from_rfc3339(created).is_ok(),
        "created_at is RFC 3339: {created:?}"
    );
    assert_eq!(
        inserted.data["updated_at"], inserted.data["created_at"],
        "one instant stamps both timestamp columns"
    );
    assert_eq!(
        svc.count(
            "conf_rl",
            &[
                eq("key", serde_json::json!("user:2")),
                filt("updated_at", FilterOp::LessThan, serde_json::json!(before)),
            ],
        )
        .await
        .unwrap(),
        0,
        "a row stamped after {before} compares as later than it (stored {created:?})"
    );
}

/// The windowed counter is keyed by whichever column `conflict_columns`
/// names, takes that column's value from `data`, and refuses any request
/// part it would not write: a second conflict column, or a data field other
/// than `id` and the conflict column.
async fn check_upsert_windowed_counter_honours_its_columns(svc: &dyn DatabaseService) {
    let mut ip_col = Column::new("ip", DataType::Text);
    ip_col.nullable = true;
    ip_col.unique = true;
    let table = Table {
        name: "conf_rl_ip".to_string(),
        columns: vec![
            pk("id"),
            ip_col,
            Column::new("key", DataType::Text).null(),
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

    let now: i64 = 1_700_000_000;
    let spec = |data: &[(&str, serde_json::Value)], conflict: &[&str]| UpsertSpec {
        data: data
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect(),
        conflict_columns: conflict.iter().map(|c| (*c).to_string()).collect(),
        on_conflict: UpsertConflict::WindowedCounter {
            count_field: "count".into(),
            window_field: "window_start".into(),
            now,
            window_cutoff: now - 60,
            created_fields: vec!["created_at".into()],
            updated_fields: vec!["updated_at".into()],
        },
    };

    svc.upsert(
        "conf_rl_ip",
        spec(
            &[
                ("id", serde_json::json!("r1")),
                ("ip", serde_json::json!("10.0.0.1")),
            ],
            &["ip"],
        ),
    )
    .await
    .expect("a counter keyed by `ip` takes its value from data[\"ip\"]");
    let stored = svc.get("conf_rl_ip", "r1").await.expect("get r1");
    assert_eq!(stored.data["ip"], serde_json::json!("10.0.0.1"));
    assert_eq!(field_i64(&stored, "count"), 1);

    assert_invalid_argument(
        svc.upsert(
            "conf_rl_ip",
            spec(
                &[
                    ("id", serde_json::json!("r2")),
                    ("key", serde_json::json!("a")),
                    ("ip", serde_json::json!("b")),
                ],
                &["ip"],
            ),
        )
        .await,
        "a data field the counter would not write",
    );
    assert_invalid_argument(
        svc.upsert(
            "conf_rl_ip",
            spec(
                &[
                    ("id", serde_json::json!("r3")),
                    ("ip", serde_json::json!("c")),
                    ("key", serde_json::json!("d")),
                ],
                &["ip", "key"],
            ),
        )
        .await,
        "a second conflict column",
    );
    assert_invalid_argument(
        svc.upsert(
            "conf_rl_ip",
            spec(
                &[("id", serde_json::json!(4)), ("ip", serde_json::json!("e"))],
                &["ip"],
            ),
        )
        .await,
        "a non-string id",
    );
    assert_eq!(
        svc.count("conf_rl_ip", &[]).await.unwrap(),
        1,
        "no refused request wrote a row"
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
                cast_as: None,
            },
            AggregateColumnSpec::Avg {
                field: "amount".into(),
                alias: "mean".into(),
                cast_as: None,
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
// aggregate over BIGINT money — cast_as, SumWhere, column-to-column filters
// ---------------------------------------------------------------------------

async fn check_aggregate_money(svc: &dyn DatabaseService) {
    // Money in minor units, in `BIGINT` columns — the shape on which Postgres
    // widens `SUM` to `NUMERIC`. One amount exceeds `i32::MAX`, so an `INT4`
    // path would overflow rather than pass by accident.
    //   a: o1 total 1000 refunded    0
    //      o2 total 2000 refunded 2500   (refunded > total)
    //   b: o3 total 3_000_000_000 refunded 3_000_000_000   (refunded = total)
    let table = Table {
        name: "conf_money".to_string(),
        columns: vec![
            pk("id"),
            Column::new("account", DataType::Text),
            Column::new("total_cents", DataType::Int64),
            Column::new("refunded_cents", DataType::Int64),
        ],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    };
    reset(svc, &table).await;
    for (id, account, total, refunded) in [
        ("o1", "a", 1000_i64, 0_i64),
        ("o2", "a", 2000, 2500),
        ("o3", "b", 3_000_000_000, 3_000_000_000),
    ] {
        svc.create(
            "conf_money",
            row([
                ("id", serde_json::json!(id)),
                ("account", serde_json::json!(account)),
                ("total_cents", serde_json::json!(total)),
                ("refunded_cents", serde_json::json!(refunded)),
            ]),
        )
        .await
        .expect("seed conf_money");
    }

    let over_refunded = || {
        vec![FilterTree::ColumnCompare(ColumnFilter {
            field: "refunded_cents".into(),
            operator: ColumnCompareOp::GreaterThan,
            column: "total_cents".into(),
        })]
    };
    let spec = AggregateSpec {
        select_columns: vec!["account".into()],
        aggregates: vec![
            // `CAST(SUM(<bigint>) AS BIGINT)` — an integer on every backend.
            // Uncast, Postgres returns `NUMERIC`, which decodes as a JSON
            // float, and `field_i64` (`as_i64`) rejects a float.
            AggregateColumnSpec::Sum {
                field: "total_cents".into(),
                alias: "gross".into(),
                cast_as: Some(CastType::BigInt),
            },
            // `SumWhere`: refunds on rows that carry one.
            AggregateColumnSpec::SumWhere {
                field: "refunded_cents".into(),
                when: vec![FilterTree::Leaf(filt(
                    "refunded_cents",
                    FilterOp::GreaterThan,
                    serde_json::json!(0),
                ))],
                alias: "refunded".into(),
                cast_as: Some(CastType::BigInt),
            },
            // `SumWhere` over a column-to-column predicate: the totals of the
            // over-refunded rows. Account b's only row matches nothing, so it
            // must sum to 0, not NULL.
            AggregateColumnSpec::SumWhere {
                field: "total_cents".into(),
                when: over_refunded(),
                alias: "over_refunded_total".into(),
                cast_as: Some(CastType::BigInt),
            },
            // `CaseWhenSum` over the same column-to-column predicate.
            AggregateColumnSpec::CaseWhenSum {
                when: over_refunded(),
                alias: "over_refunded_orders".into(),
            },
            // `CAST(AVG(<bigint>) AS DOUBLE PRECISION)` — a float everywhere.
            AggregateColumnSpec::Avg {
                field: "total_cents".into(),
                alias: "mean".into(),
                cast_as: Some(CastType::Double),
            },
        ],
        filters: vec![],
        group_by: vec![GroupBySpec::Column("account".into())],
        sort: vec![SortField {
            field: "account".into(),
            desc: false,
        }],
        limit: 0,
    };
    let groups = svc
        .aggregate("conf_money", spec)
        .await
        .expect("aggregate with casts, SumWhere and a column-to-column predicate");
    assert_eq!(groups.len(), 2, "two account groups");
    let expect = [
        ("a", 3000_i64, 2500_i64, 2000_i64, 1_i64, 1500.0_f64),
        ("b", 3_000_000_000, 3_000_000_000, 0, 0, 3_000_000_000.0),
    ];
    for (grp, (account, gross, refunded, over_total, over_orders, mean)) in
        groups.iter().zip(expect)
    {
        assert_eq!(grp.data["account"], serde_json::json!(account), "group key");
        assert_eq!(field_i64(grp, "gross"), gross, "cast Sum for {account}");
        assert_eq!(
            field_i64(grp, "refunded"),
            refunded,
            "SumWhere for {account}"
        );
        assert_eq!(
            field_i64(grp, "over_refunded_total"),
            over_total,
            "column-compare SumWhere for {account}"
        );
        assert_eq!(
            field_i64(grp, "over_refunded_orders"),
            over_orders,
            "column-compare CaseWhenSum for {account}"
        );
        assert!(
            (field_f64(grp, "mean") - mean).abs() < 1e-6,
            "cast Avg for {account}"
        );
    }

    // Ungrouped, over no rows at all: `SUM` of nothing is NULL, so the
    // conditional sum and count must still come back as the integer 0.
    let empty = svc
        .aggregate(
            "conf_money",
            AggregateSpec {
                select_columns: vec![],
                aggregates: vec![
                    AggregateColumnSpec::SumWhere {
                        field: "refunded_cents".into(),
                        when: vec![FilterTree::Leaf(filt(
                            "refunded_cents",
                            FilterOp::GreaterThan,
                            serde_json::json!(0),
                        ))],
                        alias: "refunded".into(),
                        cast_as: Some(CastType::BigInt),
                    },
                    AggregateColumnSpec::CaseWhenSum {
                        when: over_refunded(),
                        alias: "over_refunded_orders".into(),
                    },
                ],
                filters: vec![eq("account", serde_json::json!("nobody"))],
                group_by: vec![],
                sort: vec![],
                limit: 0,
            },
        )
        .await
        .expect("ungrouped aggregate over no rows");
    assert_eq!(empty.len(), 1, "an ungrouped aggregate is one row");
    assert_eq!(field_i64(&empty[0], "refunded"), 0, "SumWhere over no rows");
    assert_eq!(
        field_i64(&empty[0], "over_refunded_orders"),
        0,
        "CaseWhenSum over no rows"
    );

    // A column-to-column predicate as a `list` filter, alone and inside an OR
    // group: `>=` catches the equal row the strict `>` above excludes.
    let opts = ListOptions {
        sort: vec![SortField {
            field: "id".into(),
            desc: false,
        }],
        filter_tree: Some(vec![FilterTree::ColumnCompare(ColumnFilter {
            field: "refunded_cents".into(),
            operator: ColumnCompareOp::GreaterEqual,
            column: "total_cents".into(),
        })]),
        ..ListOptions::default()
    };
    let listed = svc
        .list("conf_money", &opts)
        .await
        .expect("list with a column-to-column filter");
    let ids: Vec<&str> = listed.records.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["o2", "o3"], "refunded >= total");
    assert_eq!(
        listed.total_count, 2,
        "total_count honours the column filter"
    );

    let opts = ListOptions {
        sort: vec![SortField {
            field: "id".into(),
            desc: false,
        }],
        filter_tree: Some(vec![FilterTree::Any(vec![
            FilterTree::ColumnCompare(ColumnFilter {
                field: "refunded_cents".into(),
                operator: ColumnCompareOp::LessThan,
                column: "total_cents".into(),
            }),
            FilterTree::Leaf(eq("account", serde_json::json!("b"))),
        ])]),
        ..ListOptions::default()
    };
    let listed = svc
        .list("conf_money", &opts)
        .await
        .expect("list with a column-to-column filter in an OR group");
    let ids: Vec<&str> = listed.records.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["o1", "o3"], "refunded < total OR account = b");
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

// ---------------------------------------------------------------------------
// Names are used verbatim; reads never reshape a table
// ---------------------------------------------------------------------------

/// Assert `result` is [`DatabaseError::InvalidArgument`]; `what` labels it.
fn assert_invalid_argument<T: std::fmt::Debug>(result: Result<T, DatabaseError>, what: &str) {
    match result {
        Err(DatabaseError::InvalidArgument(_)) => {}
        other => panic!("{what}: expected InvalidArgument, got {other:?}"),
    }
}

/// A table or column name that is not a plain identifier is refused, never
/// rewritten into another name; and a filter, sort, projection or guard
/// naming a column the table lacks is refused without adding it.
///
/// `conf_a-b` is the name that, stripped of its `-`, is the existing table
/// `conf_ab`: an executor that rewrote names would answer with `conf_ab`'s
/// rows.
async fn check_names_are_verbatim_and_reads_never_reshape(svc: &dyn DatabaseService) {
    let t = "conf_ab";
    reset(svc, &crud_table(t)).await;
    svc.create(
        t,
        row([
            ("id", serde_json::json!("r1")),
            ("name", serde_json::json!("a")),
        ]),
    )
    .await
    .expect("seed conf_ab");
    let columns = svc.schema_columns(t).await.expect("schema_columns");

    let twin = "conf_a-b";
    // Another spelling of the same table (SQLite folds case) is refused too:
    // there is one name per table on every backend.
    assert_invalid_argument(
        svc.list("CONF_AB", &ListOptions::default()).await,
        "list CONF_AB",
    );
    assert_invalid_argument(
        svc.schema_drop_table(twin).await,
        "schema_drop_table conf_a-b",
    );
    assert_invalid_argument(
        svc.schema_add_column(twin, &Column::new("extra", DataType::Text).null())
            .await,
        "schema_add_column conf_a-b",
    );
    assert_invalid_argument(
        svc.schema_add_column(t, &Column::new("Extra", DataType::Text).null())
            .await,
        "schema_add_column Extra",
    );
    assert_invalid_argument(
        svc.upsert(
            t,
            UpsertSpec {
                data: vec![
                    ("id".to_string(), serde_json::json!("r1")),
                    ("Name".to_string(), serde_json::json!("u")),
                ],
                conflict_columns: vec!["id".to_string()],
                on_conflict: UpsertConflict::SetColumns(vec!["Name".to_string()]),
            },
        )
        .await,
        "upsert setting Name",
    );
    assert_invalid_argument(
        svc.list(twin, &ListOptions::default()).await,
        "list conf_a-b",
    );
    assert_invalid_argument(svc.count(twin, &[]).await, "count conf_a-b");
    assert_invalid_argument(svc.get(twin, "r1").await, "get conf_a-b");
    assert_invalid_argument(
        svc.create(twin, row([("name", serde_json::json!("b"))]))
            .await,
        "create conf_a-b",
    );
    assert_invalid_argument(svc.delete(twin, "r1").await, "delete conf_a-b");
    assert_invalid_argument(
        svc.create(t, row([("no-te", serde_json::json!("x"))]))
            .await,
        "create with a non-identifier data key",
    );

    let unknown = || vec![eq("zz", serde_json::json!("x"))];
    assert_invalid_argument(
        svc.list(
            t,
            &ListOptions {
                sort: vec![SortField {
                    field: "zz".to_string(),
                    desc: false,
                }],
                ..ListOptions::default()
            },
        )
        .await,
        "list sorted on an unknown column",
    );
    assert_invalid_argument(
        svc.list(
            t,
            &ListOptions {
                filters: unknown(),
                ..ListOptions::default()
            },
        )
        .await,
        "list filtered on an unknown column",
    );
    assert_invalid_argument(
        svc.list(
            t,
            &ListOptions {
                columns: Some(vec!["zz".to_string()]),
                ..ListOptions::default()
            },
        )
        .await,
        "list projecting an unknown column",
    );
    assert_invalid_argument(svc.count(t, &unknown()).await, "count on an unknown column");
    assert_invalid_argument(
        svc.take_where(t, &unknown()).await,
        "take_where on an unknown column",
    );
    assert_invalid_argument(
        svc.delete_where_count(t, &unknown()).await,
        "delete_where_count on an unknown column",
    );
    assert_invalid_argument(
        svc.update_where_count(t, &unknown(), row([("note", serde_json::json!("n"))]))
            .await,
        "update_where_count on an unknown column",
    );
    assert_invalid_argument(
        svc.batch(vec![WriteOp::DeleteWhere {
            collection: t.to_string(),
            filters: unknown(),
        }])
        .await,
        "batch delete-where on an unknown column",
    );
    assert_invalid_argument(
        svc.insert_guarded(
            t,
            row([("name", serde_json::json!("g"))]),
            &[CapGuard::CountBelow {
                filters: unknown(),
                cap: 10,
            }],
        )
        .await,
        "insert_guarded with a guard on an unknown column",
    );

    assert_invalid_argument(svc.sum(t, "zz", &[]).await, "sum of an unknown column");
    let aggregate =
        |aggregates: Vec<AggregateColumnSpec>, group_by: Vec<GroupBySpec>| AggregateSpec {
            select_columns: Vec::new(),
            aggregates,
            filters: Vec::new(),
            group_by,
            sort: Vec::new(),
            limit: 0,
        };
    assert_invalid_argument(
        svc.aggregate(
            t,
            aggregate(
                vec![AggregateColumnSpec::Max {
                    field: "zz".to_string(),
                    alias: "m".to_string(),
                }],
                Vec::new(),
            ),
        )
        .await,
        "aggregate over an unknown column",
    );
    assert_invalid_argument(
        svc.aggregate(
            t,
            aggregate(
                vec![AggregateColumnSpec::Count {
                    alias: "c".to_string(),
                }],
                vec![GroupBySpec::Column("zz".to_string())],
            ),
        )
        .await,
        "aggregate grouped by an unknown column",
    );
    assert_invalid_argument(
        svc.aggregate(
            t,
            aggregate(
                vec![AggregateColumnSpec::Count {
                    alias: "Total".to_string(),
                }],
                Vec::new(),
            ),
        )
        .await,
        "aggregate with a non-identifier alias",
    );

    assert_eq!(
        svc.schema_columns(t).await.expect("schema_columns"),
        columns,
        "no refused request added a column"
    );
    let rows = svc
        .list(t, &ListOptions::default())
        .await
        .expect("list conf_ab");
    let ids: Vec<&str> = rows.records.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["r1"], "no refused request changed a row");
}

/// A name longer than PostgreSQL's 63-byte identifier limit is refused: the
/// server would keep its first 63 bytes and so reach the table of that
/// shorter name.
async fn check_names_longer_than_postgres_keeps_are_refused(svc: &dyn DatabaseService) {
    let kept = format!("conf_{}", "l".repeat(58));
    assert_eq!(kept.len(), 63);
    reset(svc, &crud_table(&kept)).await;
    svc.create(
        &kept,
        row([
            ("id", serde_json::json!("r1")),
            ("name", serde_json::json!("a")),
        ]),
    )
    .await
    .expect("seed the 63-byte table");

    let longer = format!("{kept}x");
    assert_invalid_argument(
        svc.list(&longer, &ListOptions::default()).await,
        "list a 64-byte name",
    );
    assert_invalid_argument(
        svc.create(&longer, row([("name", serde_json::json!("b"))]))
            .await,
        "create in a 64-byte name",
    );
    let long_column = "c".repeat(64);
    assert_invalid_argument(
        svc.count(&kept, &[eq(&long_column, serde_json::json!("x"))])
            .await,
        "count filtered on a 64-byte column",
    );
    assert_eq!(
        svc.count(&kept, &[]).await.expect("count"),
        1,
        "nothing reached the 63-byte table"
    );
    svc.schema_drop_table(&kept)
        .await
        .expect("drop the 63-byte table");
}
