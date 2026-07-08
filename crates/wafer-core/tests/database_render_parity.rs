//! Wire-round-trip render-parity tests for the SP-B1 structured ops.
//!
//! SP-B1 introduces the typed `DATABASE_UPSERT` / `DATABASE_AGGREGATE` ops and
//! the `FilterNode` predicate tree / column-projection additions to
//! `DATABASE_LIST` / `DATABASE_UPDATE_WHERE`. SP-B2 will migrate the ~33
//! solobase execute/query sites onto these ops. This file proves that
//! migration is a **pure re-plumbing**: for every migrated shape, feeding
//! inputs through the real transport + handler conversion path
//!
//! ```text
//! wire request  --codec(msgpack)-->  bytes  --codec-->  wire request
//!   --{to_upsert_spec | to_aggregate_spec | convert_filter_tree}-->  builder input
//!   --wafer_sql_utils builder-->  Statement
//! ```
//!
//! renders **byte-identical SQL and identical bound parameters** to the direct
//! `wafer_sql_utils` builder call a consumer writes by hand today. Both halves
//! are produced independently — the "direct" half hand-builds the
//! builder-input types (`Filter` / `GroupedQueryConfig` / plain data pairs)
//! exactly as current solobase sites do; the "via-wire" half constructs the
//! wire request, serialises + deserialises it through [`codec`] (the same
//! MessagePack transport the runtime uses), and runs it through the *same*
//! conversion the host handler calls before reaching the builder. A tautology
//! (`assert_eq!(direct, direct)`) would prove nothing; here the two Statements
//! come from two independent constructions, so the assertion actually pins the
//! wire→builder conversion.
//!
//! Every shape is covered for both `Backend::Sqlite` and `Backend::Postgres`,
//! since the two dialects differ in placeholder syntax and the migration must
//! hold for both.

use std::collections::HashMap;

use serde_json::json;
use wafer_block::{
    codec,
    db::{Filter, FilterOp, FilterTree, ListOptions, SortField},
    wire::database as wire,
};
use wafer_core::interfaces::database::{
    handler::{convert_filter_tree, flatten_leaves, to_aggregate_spec, to_upsert_spec},
    service::UpsertConflict,
};
use wafer_sql_utils::{
    aggregate::{AggFunc, AggregateColumn, DateBucketGroup, GroupedQueryConfig},
    query, upsert,
    value::sea_values_to_json,
    Backend, Statement,
};

/// Assert two independently-produced statements render identical SQL text and
/// identical bound parameter values. Parameters are compared through
/// [`sea_values_to_json`] (the same conversion the execution layer applies
/// before binding) so the comparison is on plain JSON values rather than
/// relying on `sea_query::Value`'s `PartialEq`.
fn assert_stmt_parity(direct: &Statement, via_wire: &Statement, ctx: &str) {
    assert_eq!(
        direct.sql, via_wire.sql,
        "SQL text diverged between the direct builder call and the wire round-trip [{ctx}]"
    );
    assert_eq!(
        sea_values_to_json(direct.values.clone()),
        sea_values_to_json(via_wire.values.clone()),
        "bound parameter values diverged between direct and wire paths [{ctx}]"
    );
    // A statement whose SQL references N placeholders but binds 0 params (or
    // vice versa) would still slip through an SQL-only check on some inputs;
    // pin the count too as a cheap independent invariant.
    assert_eq!(
        direct.values.len(),
        via_wire.values.len(),
        "bound parameter count diverged [{ctx}]"
    );
}

/// Mirror the handler's `convert_sort` (wire `SortFieldDef` -> `SortField`).
fn convert_sort(defs: Vec<wire::SortFieldDef>) -> Vec<SortField> {
    defs.into_iter()
        .map(|s| SortField {
            field: s.field,
            desc: s.desc,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// update_where — FilterNode round-trip -> flatten_leaves -> build_update_where
// ---------------------------------------------------------------------------

fn update_where_parity(backend: Backend) {
    // Logical `SET` data is held identical on both sides: it is a plain
    // `HashMap`/pair list on the wire and is *not* wire-transformed (the
    // service layer's timestamp-stamp + sorted-pairs step is identical
    // regardless of transport). The only wire-specific transform for this op
    // is the predicate: `Vec<FilterNode>` -> `convert_filter_tree` ->
    // `flatten_leaves` -> `Vec<Filter>`. Isolating the filter path is exactly
    // what makes the assertion meaningful.
    let data: Vec<(String, serde_json::Value)> = vec![
        ("status".to_string(), json!("done")),
        ("attempts".to_string(), json!(3)),
    ];

    // (a) DIRECT — hand-built `Filter`s, as a solobase update_where site writes today.
    let filters_direct = vec![
        Filter {
            field: "id".to_string(),
            operator: FilterOp::Equal,
            value: json!("row-1"),
        },
        Filter {
            field: "tenant".to_string(),
            operator: FilterOp::NotEqual,
            value: json!("system"),
        },
    ];
    let direct = query::build_update_where("things", &data, &filters_direct, backend);

    // (b) VIA WIRE — construct the request, round-trip through the codec, then
    //     run the *same* conversion the `DATABASE_UPDATE_WHERE` handler arm runs.
    let req = wire::UpdateWhereRequest {
        collection: "things".to_string(),
        filters: vec![
            wire::FilterNode::Leaf(wire::FilterDef {
                field: "id".to_string(),
                operator: "eq".to_string(),
                value: json!("row-1"),
            }),
            wire::FilterNode::Leaf(wire::FilterDef {
                field: "tenant".to_string(),
                operator: "neq".to_string(),
                value: json!("system"),
            }),
        ],
        data: HashMap::from([
            ("status".to_string(), json!("done")),
            ("attempts".to_string(), json!(3)),
        ]),
    };
    let bytes = codec::encode(&req).expect("encode UpdateWhereRequest");
    let decoded: wire::UpdateWhereRequest =
        codec::decode(&bytes).expect("decode UpdateWhereRequest");
    let tree = convert_filter_tree(decoded.filters).expect("convert filter tree");
    let filters_wire = flatten_leaves(&tree).expect("flatten leaves");
    let via = query::build_update_where("things", &data, &filters_wire, backend);

    assert_stmt_parity(&direct, &via, "update_where");
}

#[test]
fn update_where_render_parity_sqlite() {
    update_where_parity(Backend::Sqlite);
}

#[test]
fn update_where_render_parity_postgres() {
    update_where_parity(Backend::Postgres);
}

// ---------------------------------------------------------------------------
// list projection — FilterNode OR-group round-trip -> build_condition_tree
//                   -> build_select_columns
// ---------------------------------------------------------------------------

fn list_projection_parity(backend: Backend) {
    let columns = vec!["id".to_string(), "name".to_string()];
    let col_refs: Vec<&str> = columns.iter().map(String::as_str).collect();

    // (a) DIRECT — hand-built `FilterTree` with an OR group + a projected SELECT.
    let tree_direct = vec![FilterTree::Any(vec![
        FilterTree::Leaf(Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: json!("active"),
        }),
        FilterTree::Leaf(Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: json!("pending"),
        }),
    ])];
    let opts_direct = ListOptions {
        filters: Vec::new(),
        sort: vec![SortField {
            field: "name".to_string(),
            desc: false,
        }],
        limit: 25,
        offset: 50,
        skip_count: false,
        filter_tree: Some(tree_direct.clone()),
        columns: Some(columns.clone()),
    };
    let extra_direct = query::build_condition_tree(&tree_direct);
    let direct =
        query::build_select_columns("things", &col_refs, &opts_direct, extra_direct, backend);

    // (b) VIA WIRE — mirror the `DATABASE_LIST` handler arm: filters flow only
    //     through `filter_tree` (flat `opts.filters` stays empty), rendered as
    //     the `extra_condition` of the projected SELECT.
    let req = wire::ListRequest {
        collection: "things".to_string(),
        filters: vec![wire::FilterNode::Any {
            any: vec![
                wire::FilterNode::Leaf(wire::FilterDef {
                    field: "status".to_string(),
                    operator: "eq".to_string(),
                    value: json!("active"),
                }),
                wire::FilterNode::Leaf(wire::FilterDef {
                    field: "status".to_string(),
                    operator: "eq".to_string(),
                    value: json!("pending"),
                }),
            ],
        }],
        sort: vec![wire::SortFieldDef {
            field: "name".to_string(),
            desc: false,
        }],
        limit: 25,
        offset: 50,
        skip_count: false,
        columns: Some(columns.clone()),
    };
    let bytes = codec::encode(&req).expect("encode ListRequest");
    let decoded: wire::ListRequest = codec::decode(&bytes).expect("decode ListRequest");
    let tree = convert_filter_tree(decoded.filters).expect("convert filter tree");
    let opts_wire = ListOptions {
        filters: Vec::new(),
        sort: convert_sort(decoded.sort),
        limit: decoded.limit,
        offset: decoded.offset,
        skip_count: decoded.skip_count,
        filter_tree: Some(tree),
        columns: decoded.columns.clone(),
    };
    let extra_wire = opts_wire
        .filter_tree
        .as_deref()
        .and_then(query::build_condition_tree);
    let via_cols_owned = decoded.columns.expect("columns present");
    let via_cols: Vec<&str> = via_cols_owned.iter().map(String::as_str).collect();
    let via = query::build_select_columns("things", &via_cols, &opts_wire, extra_wire, backend);

    assert_stmt_parity(&direct, &via, "list_projection");
}

#[test]
fn list_projection_render_parity_sqlite() {
    list_projection_parity(Backend::Sqlite);
}

#[test]
fn list_projection_render_parity_postgres() {
    list_projection_parity(Backend::Postgres);
}

// ---------------------------------------------------------------------------
// upsert (SetColumns) — UpsertRequest round-trip -> to_upsert_spec -> build_upsert
// ---------------------------------------------------------------------------

fn upsert_set_columns_parity(backend: Backend) {
    // Shared logical inputs.
    let data: Vec<(String, serde_json::Value)> = vec![
        ("id".to_string(), json!("w1")),
        ("name".to_string(), json!("gizmo")),
        ("qty".to_string(), json!(7)),
    ];

    // (a) DIRECT — the raw builder call a consumer's upsert-by-field site makes.
    let direct = upsert::build_upsert("widgets", &data, &["id"], &["name", "qty"], backend);

    // (b) VIA WIRE.
    let req = wire::UpsertRequest {
        collection: "widgets".to_string(),
        data: vec![
            ("id".to_string(), json!("w1")),
            ("name".to_string(), json!("gizmo")),
            ("qty".to_string(), json!(7)),
        ],
        conflict_columns: vec!["id".to_string()],
        on_conflict: wire::OnConflict::SetColumns(vec!["name".to_string(), "qty".to_string()]),
    };
    let bytes = codec::encode(&req).expect("encode UpsertRequest");
    let decoded: wire::UpsertRequest = codec::decode(&bytes).expect("decode UpsertRequest");
    let (collection, spec) = to_upsert_spec(decoded).expect("to_upsert_spec");

    // Mirror `DbExec::upsert`'s SetColumns arm (the rendering-dispatch is a few
    // lines; the wire->spec *conversion* under test is `to_upsert_spec`).
    let conflict: Vec<&str> = spec.conflict_columns.iter().map(String::as_str).collect();
    let UpsertConflict::SetColumns(update_cols) = &spec.on_conflict else {
        panic!("expected SetColumns conflict resolution");
    };
    let update: Vec<&str> = update_cols.iter().map(String::as_str).collect();
    let via = upsert::build_upsert(&collection, &spec.data, &conflict, &update, backend);

    assert_stmt_parity(&direct, &via, "upsert_set_columns");
}

#[test]
fn upsert_set_columns_render_parity_sqlite() {
    upsert_set_columns_parity(Backend::Sqlite);
}

#[test]
fn upsert_set_columns_render_parity_postgres() {
    upsert_set_columns_parity(Backend::Postgres);
}

// ---------------------------------------------------------------------------
// upsert (WindowedCounter) — UpsertRequest round-trip -> to_upsert_spec
//                            -> build_windowed_counter_upsert
// ---------------------------------------------------------------------------

fn upsert_windowed_counter_parity(backend: Backend) {
    // The created/updated split is the interesting bit: `created_fields` are
    // stamped INSERT-only, `updated_fields` are re-stamped on conflict. Both
    // paths must render that split identically.
    let now = 1_700_000_000i64;
    let window_cutoff = 1_699_999_940i64;

    // (a) DIRECT — the raw windowed-counter builder call a rate-limit site makes.
    let direct = upsert::build_windowed_counter_upsert(
        "rate_limits",
        "key",
        "rl-1",
        "user:1:login",
        "count",
        "window_start",
        &["created_at"],
        &["updated_at", "seen_at"],
        now,
        window_cutoff,
        backend,
    )
    .expect("direct windowed-counter build");

    // (b) VIA WIRE.
    let req = wire::UpsertRequest {
        collection: "rate_limits".to_string(),
        data: vec![
            ("id".to_string(), json!("rl-1")),
            ("key".to_string(), json!("user:1:login")),
        ],
        conflict_columns: vec!["key".to_string()],
        on_conflict: wire::OnConflict::WindowedCounter {
            count_field: "count".to_string(),
            window_field: "window_start".to_string(),
            now,
            window_cutoff,
            created_fields: vec!["created_at".to_string()],
            updated_fields: vec!["updated_at".to_string(), "seen_at".to_string()],
        },
    };
    let bytes = codec::encode(&req).expect("encode UpsertRequest");
    let decoded: wire::UpsertRequest = codec::decode(&bytes).expect("decode UpsertRequest");
    let (collection, spec) = to_upsert_spec(decoded).expect("to_upsert_spec");

    // Mirror `DbExec::upsert`'s WindowedCounter arm.
    let UpsertConflict::WindowedCounter {
        count_field,
        window_field,
        now: spec_now,
        window_cutoff: spec_cutoff,
        created_fields,
        updated_fields,
    } = &spec.on_conflict
    else {
        panic!("expected WindowedCounter conflict resolution");
    };
    // `id`/`key` are read from the insert data (matches `extract_windowed_id_key`).
    let id = spec
        .data
        .iter()
        .find(|(k, _)| k == "id")
        .and_then(|(_, v)| v.as_str())
        .expect("id string in data");
    let key = spec
        .data
        .iter()
        .find(|(k, _)| k == "key")
        .and_then(|(_, v)| v.as_str())
        .expect("key string in data");
    let conflict_col = spec.conflict_columns.first().map_or("key", String::as_str);
    let created: Vec<&str> = created_fields.iter().map(String::as_str).collect();
    let updated: Vec<&str> = updated_fields.iter().map(String::as_str).collect();
    let via = upsert::build_windowed_counter_upsert(
        &collection,
        conflict_col,
        id,
        key,
        count_field,
        window_field,
        &created,
        &updated,
        *spec_now,
        *spec_cutoff,
        backend,
    )
    .expect("wire windowed-counter build");

    assert_stmt_parity(&direct, &via, "upsert_windowed_counter");
}

#[test]
fn upsert_windowed_counter_render_parity_sqlite() {
    upsert_windowed_counter_parity(Backend::Sqlite);
}

#[test]
fn upsert_windowed_counter_render_parity_postgres() {
    upsert_windowed_counter_parity(Backend::Postgres);
}

// ---------------------------------------------------------------------------
// aggregate — AggregateRequest round-trip -> to_aggregate_spec
//             -> into_grouped_config -> build_grouped_query
//
// Covers, in one request, the three distinct aggregate features: a grouped
// Count, a CaseWhenSum conditional count, and a DateBucket group — the exact
// shape `solobase admin/pages/network.rs` builds a `GroupedQueryConfig` for by
// hand today.
// ---------------------------------------------------------------------------

fn aggregate_parity(backend: Backend) {
    // (a) DIRECT — hand-built `GroupedQueryConfig`, mirroring what
    //     `into_grouped_config` must reproduce field-for-field.
    let when_direct = vec![FilterTree::Leaf(Filter {
        field: "status".to_string(),
        operator: FilterOp::GreaterEqual,
        value: json!(400),
    })];
    let cfg_direct = GroupedQueryConfig {
        table: "request_logs".to_string(),
        select_columns: vec!["method".to_string()],
        aggregates: vec![
            AggregateColumn {
                func: AggFunc::Count,
                field: None,
                alias: "cnt".to_string(),
                cast_as: None,
                inner_expr: None,
            },
            AggregateColumn::case_when_sum("errors", query::tree_to_simple_expr(&when_direct)),
        ],
        filters: vec![Filter {
            field: "active".to_string(),
            operator: FilterOp::Equal,
            value: json!(true),
        }],
        group_by: vec!["method".to_string()],
        date_buckets: vec![DateBucketGroup {
            field: "created_at".to_string(),
            alias: "created_at".to_string(),
        }],
        order_by: vec![SortField {
            field: "cnt".to_string(),
            desc: true,
        }],
        limit: Some(50),
    };
    let direct = wafer_sql_utils::aggregate::build_grouped_query(cfg_direct, backend);

    // (b) VIA WIRE.
    let req = wire::AggregateRequest {
        collection: "request_logs".to_string(),
        select_columns: vec!["method".to_string()],
        aggregates: vec![
            wire::AggregateColumnDef::Count {
                alias: "cnt".to_string(),
            },
            wire::AggregateColumnDef::CaseWhenSum {
                when: vec![wire::FilterNode::Leaf(wire::FilterDef {
                    field: "status".to_string(),
                    operator: "gte".to_string(),
                    value: json!(400),
                })],
                alias: "errors".to_string(),
            },
        ],
        filters: vec![wire::FilterNode::Leaf(wire::FilterDef {
            field: "active".to_string(),
            operator: "eq".to_string(),
            value: json!(true),
        })],
        group_by: vec![
            wire::GroupByDef::Column("method".to_string()),
            wire::GroupByDef::DateBucket {
                field: "created_at".to_string(),
            },
        ],
        sort: vec![wire::SortFieldDef {
            field: "cnt".to_string(),
            desc: true,
        }],
        limit: 50,
    };
    let bytes = codec::encode(&req).expect("encode AggregateRequest");
    let decoded: wire::AggregateRequest = codec::decode(&bytes).expect("decode AggregateRequest");
    let (collection, spec) = to_aggregate_spec(decoded).expect("to_aggregate_spec");
    // Server-side render (the `!Send` `GroupedQueryConfig` is built + consumed
    // in one expression, exactly as `DbExec::aggregate` does).
    let cfg_wire = spec.into_grouped_config(collection);
    let via = wafer_sql_utils::aggregate::build_grouped_query(cfg_wire, backend);

    assert_stmt_parity(&direct, &via, "aggregate");
}

#[test]
fn aggregate_render_parity_sqlite() {
    aggregate_parity(Backend::Sqlite);
}

#[test]
fn aggregate_render_parity_postgres() {
    aggregate_parity(Backend::Postgres);
}
