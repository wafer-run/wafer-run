//! Pins the exact SQL and parameters every builder family renders, for both
//! dialects (SQLite — which D1 also speaks — and PostgreSQL).
//!
//! Builder output is a contract with every database backend: a change in
//! quoting, placeholder numbering, parenthesisation, parameter order or
//! parameter type reaches every caller at once. The unit tests next to each
//! builder mostly assert fragments (`contains`); this test compares the whole
//! rendering against `tests/golden/generated_sql.txt`, so any drift — from an
//! edit here or from a sea-query upgrade — fails it.
//!
//! Each parameter is pinned twice: as the JSON the database backends bind
//! (`sea_values_to_json`, the only form they consume) and as the sea-query
//! `Value` variant it was built as.
//!
//! After an intended change, regenerate the golden file with
//! `WAFER_SQL_PIN_BLESS=1 cargo test -p wafer-sql-utils --test generated_sql_pin`
//! and review the diff.

use std::fmt::Write as _;

use serde_json::{json, Value as Json};
use wafer_block::db::{
    ColumnCompareOp, ColumnFilter, Filter, FilterOp, FilterTree, ListOptions, SortField,
};
use wafer_schema::{col_bool, col_int, col_json, col_string, default_int, pk, timestamps};
use wafer_sql_utils::{
    aggregate::{self, AggFunc, AggregateColumn, CastType, DateBucketGroup, GroupedQueryConfig},
    ddl,
    guard::{self, CapGuard},
    introspect, query, upsert,
    value::sea_values_to_json,
    vector::{self, VectorIndexRename, VectorIndexSchema},
    Backend, SeaValue, SqlBuildError, Statement,
};

const BACKENDS: [(Backend, &str); 2] =
    [(Backend::Sqlite, "sqlite"), (Backend::Postgres, "postgres")];

fn f(field: &str, operator: FilterOp, value: Json) -> Filter {
    Filter {
        field: field.into(),
        operator,
        value,
    }
}

/// One of every filter operator, with values of every JSON type a filter
/// takes.
fn every_filter() -> Vec<Filter> {
    vec![
        f("status", FilterOp::Equal, json!("active")),
        f("kind", FilterOp::NotEqual, json!("bot")),
        f("score", FilterOp::GreaterThan, json!(10)),
        f("score", FilterOp::GreaterEqual, json!(-3)),
        f("ratio", FilterOp::LessThan, json!(0.75)),
        f("ratio", FilterOp::LessEqual, json!(1)),
        f("name", FilterOp::Like, json!("a%_b")),
        f("email", FilterOp::ContainsIgnoreCase, json!("Ex_ample%")),
        f("path", FilterOp::StartsWith, json!("/a_b%")),
        f("tier", FilterOp::In, json!(["free", "pro", 3])),
        f("deleted_at", FilterOp::IsNull, Json::Null),
        f("verified_at", FilterOp::IsNotNull, Json::Null),
        f("enabled", FilterOp::Equal, json!(true)),
        f("note", FilterOp::Equal, json!("it's \"quoted\"")),
    ]
}

fn tree() -> Vec<FilterTree> {
    vec![
        FilterTree::Leaf(f("org_id", FilterOp::Equal, json!("o1"))),
        FilterTree::Any(vec![
            FilterTree::Leaf(f("status", FilterOp::In, json!(["a", "b"]))),
            FilterTree::All(vec![
                FilterTree::Leaf(f("score", FilterOp::GreaterThan, json!(5))),
                FilterTree::ColumnCompare(ColumnFilter {
                    field: "used".into(),
                    operator: ColumnCompareOp::LessThan,
                    column: "quota".into(),
                }),
            ]),
            FilterTree::Leaf(f("deleted_at", FilterOp::IsNull, Json::Null)),
        ]),
    ]
}

fn data() -> Vec<(String, Json)> {
    vec![
        ("id".into(), json!("r1")),
        ("name".into(), json!("Ann")),
        ("count".into(), json!(42)),
        ("ratio".into(), json!(0.5)),
        ("enabled".into(), json!(false)),
        ("meta".into(), json!({"k": [1, "v"]})),
        ("tags".into(), json!(["x", "y"])),
        ("cleared".into(), Json::Null),
    ]
}

fn sort() -> Vec<SortField> {
    vec![
        SortField {
            field: "created_at".into(),
            desc: true,
        },
        SortField {
            field: "name".into(),
            desc: false,
        },
    ]
}

struct Out(String);

impl Out {
    fn header(&mut self, family: &str, case: &str, backend: &str) {
        writeln!(self.0, "### {family} / {case} [{backend}]").unwrap();
    }

    fn sql_params(&mut self, sql: &str, params: &[Json]) {
        writeln!(self.0, "sql: {sql}").unwrap();
        writeln!(self.0, "params: {}", Json::Array(params.to_vec())).unwrap();
    }

    fn stmt(&mut self, family: &str, case: &str, backend: &str, s: Statement) {
        self.header(family, case, backend);
        let kinds: Vec<String> = s.values.iter().map(variant_name).collect();
        self.sql_params(&s.sql, &sea_values_to_json(s.values));
        writeln!(self.0, "kinds: [{}]", kinds.join(", ")).unwrap();
        writeln!(self.0, "collection: {}", s.collection).unwrap();
        self.0.push('\n');
    }

    fn stmt_result(
        &mut self,
        family: &str,
        case: &str,
        backend: &str,
        s: Result<Statement, SqlBuildError>,
    ) {
        match s {
            Ok(s) => self.stmt(family, case, backend, s),
            Err(e) => {
                self.header(family, case, backend);
                writeln!(self.0, "error: {e:?}\n").unwrap();
            }
        }
    }

    fn raw(&mut self, family: &str, case: &str, backend: &str, sql: &str, params: &[Json]) {
        self.header(family, case, backend);
        self.sql_params(sql, params);
        self.0.push('\n');
    }
}

/// The sea-query `Value` variant name (`BigInt`, `String`, `Json`, …): the
/// text before the first `(` of its `Debug` rendering.
fn variant_name(v: &SeaValue) -> String {
    let debug = format!("{v:?}");
    debug.split('(').next().unwrap_or_default().to_owned()
}

fn grouped_config(backend: Backend) -> GroupedQueryConfig {
    let when = query::tree_to_simple_expr(
        &[FilterTree::Leaf(f(
            "status",
            FilterOp::Equal,
            json!("paid"),
        ))],
        backend,
    );
    GroupedQueryConfig {
        table: "orders".into(),
        select_columns: vec!["region".into()],
        aggregates: vec![
            AggregateColumn {
                func: AggFunc::Count,
                field: None,
                alias: "n".into(),
                cast_as: None,
                inner_expr: None,
            },
            AggregateColumn {
                func: AggFunc::Sum,
                field: Some("amount".into()),
                alias: "total".into(),
                cast_as: Some(CastType::BigInt),
                inner_expr: None,
            },
            AggregateColumn {
                func: AggFunc::SumOrZero,
                field: Some("fee".into()),
                alias: "fees".into(),
                cast_as: None,
                inner_expr: None,
            },
            AggregateColumn {
                func: AggFunc::Avg,
                field: Some("amount".into()),
                alias: "mean".into(),
                cast_as: Some(CastType::Double),
                inner_expr: None,
            },
            AggregateColumn {
                func: AggFunc::Max,
                field: Some("amount".into()),
                alias: "hi".into(),
                cast_as: None,
                inner_expr: None,
            },
            AggregateColumn {
                func: AggFunc::Min,
                field: Some("amount".into()),
                alias: "lo".into(),
                cast_as: None,
                inner_expr: None,
            },
            AggregateColumn {
                func: AggFunc::Coalesce(json!(7)),
                field: Some("bonus".into()),
                alias: "bonus_or".into(),
                cast_as: None,
                inner_expr: None,
            },
            AggregateColumn::case_when_sum("paid_count", when.clone()),
            AggregateColumn::sum_where("paid_amount", "amount", when),
            AggregateColumn::count_groups("groups"),
        ],
        filters: vec![
            f("org_id", FilterOp::Equal, json!("o1")),
            f("amount", FilterOp::GreaterThan, json!(0)),
        ],
        group_by: vec!["region".into()],
        date_buckets: vec![DateBucketGroup {
            field: "created_at".into(),
            alias: "day".into(),
        }],
        having: vec![f("n", FilterOp::GreaterEqual, json!(2))],
        order_by: vec![SortField {
            field: "total".into(),
            desc: true,
        }],
        limit: Some(20),
        offset: 40,
    }
}

fn render_query(out: &mut Out, backend: Backend, b: &str) {
    const Q: &str = "query";
    let filters = every_filter();
    let paged = ListOptions {
        filters: filters.clone(),
        sort: sort(),
        limit: Some(25),
        offset: 50,
        ..ListOptions::default()
    };
    out.stmt_result(
        Q,
        "select_every_filter_sorted_paged",
        b,
        query::build_select("users", &paged, &["id"], backend),
    );
    out.stmt_result(
        Q,
        "select_unsorted_unique_key",
        b,
        query::build_select(
            "users",
            &ListOptions {
                limit: Some(10),
                ..ListOptions::default()
            },
            &["org_id", "id"],
            backend,
        ),
    );
    out.stmt_result(
        Q,
        "select_all",
        b,
        query::build_select("users", &ListOptions::default(), &[], backend),
    );
    // A filter and sort field reach sea-query unvalidated and are made safe
    // only by its identifier quoting, so pin how a quote inside one renders.
    out.stmt_result(
        Q,
        "select_quoted_identifier",
        b,
        query::build_select(
            "users",
            &ListOptions {
                filters: vec![f("we\"ird", FilterOp::Equal, json!(1))],
                sort: vec![SortField {
                    field: "or\"der".into(),
                    desc: false,
                }],
                ..ListOptions::default()
            },
            &[],
            backend,
        ),
    );
    out.stmt_result(
        Q,
        "select_filter_tree",
        b,
        query::build_select(
            "users",
            &ListOptions {
                filter_tree: Some(tree()),
                sort: sort(),
                ..ListOptions::default()
            },
            &["id"],
            backend,
        ),
    );
    out.stmt_result(
        Q,
        "select_with_condition",
        b,
        query::build_select_with_condition(
            "users",
            &ListOptions {
                filters: vec![f("org_id", FilterOp::Equal, json!("o1"))],
                ..ListOptions::default()
            },
            query::build_condition_tree(&tree(), backend),
            &["id"],
            backend,
        ),
    );
    out.stmt_result(
        Q,
        "select_columns",
        b,
        query::build_select_columns(
            "users",
            &["id", "email"],
            &ListOptions {
                filters: vec![f("email", FilterOp::Like, json!("%@x.io"))],
                limit: Some(5),
                ..ListOptions::default()
            },
            None,
            &["id"],
            backend,
        ),
    );
    out.stmt_result(
        Q,
        "select_zero_limit",
        b,
        query::build_select(
            "users",
            &ListOptions {
                limit: Some(0),
                ..ListOptions::default()
            },
            &[],
            backend,
        ),
    );
    out.stmt(
        Q,
        "insert",
        b,
        query::build_insert("users", &data(), backend),
    );
    out.stmt(
        Q,
        "insert_returning",
        b,
        query::build_insert_returning("users", &data(), backend),
    );
    out.stmt(
        Q,
        "select_by_id",
        b,
        query::build_select_by_id("users", "r1", backend),
    );
    out.stmt(
        Q,
        "update_by_id",
        b,
        query::build_update_by_id("users", "r1", &data()[1..], backend),
    );
    out.stmt(
        Q,
        "update_by_id_returning",
        b,
        query::build_update_by_id_returning("users", "r1", &data()[1..3], backend),
    );
    out.stmt(
        Q,
        "update_where",
        b,
        query::build_update_where("users", &data()[1..3], &filters, backend),
    );
    out.stmt(
        Q,
        "increment_field_where",
        b,
        query::build_increment_field_where("users", "count", -2, &filters[..3], backend),
    );
    out.stmt(
        Q,
        "delete_by_id",
        b,
        query::build_delete_by_id("users", "r1", backend),
    );
    out.stmt(
        Q,
        "delete_where",
        b,
        query::build_delete_where("users", &filters, backend),
    );
    out.stmt(
        Q,
        "delete_where_no_filters",
        b,
        query::build_delete_where("users", &[], backend),
    );
    out.stmt(
        Q,
        "delete_where_returning",
        b,
        query::build_delete_where_returning("users", &filters[..2], backend),
    );
}

fn render_aggregate(out: &mut Out, backend: Backend, b: &str) {
    const A: &str = "aggregate";
    let filters = every_filter();
    out.stmt(
        A,
        "count",
        b,
        aggregate::build_count("events", &filters, backend),
    );
    out.stmt(
        A,
        "count_unfiltered",
        b,
        aggregate::build_count("events", &[], backend),
    );
    out.stmt(
        A,
        "count_with_condition",
        b,
        aggregate::build_count_with_condition(
            "events",
            &filters[..1],
            query::build_condition_tree(&tree(), backend),
            backend,
        ),
    );
    out.stmt(
        A,
        "sum",
        b,
        aggregate::build_sum("events", "amount", &filters[..2], backend),
    );
    out.stmt_result(
        A,
        "daily_count",
        b,
        aggregate::build_daily_count("events", "created_at", &filters[..1], backend),
    );
    out.stmt(
        A,
        "avg",
        b,
        aggregate::build_avg("events", "amount", &filters[..2], backend),
    );
    out.stmt_result(
        A,
        "grouped_query",
        b,
        aggregate::build_grouped_query(&grouped_config(backend), backend),
    );
    out.stmt_result(
        A,
        "grouped_query_minimal",
        b,
        aggregate::build_grouped_query(
            &GroupedQueryConfig {
                table: "orders".into(),
                select_columns: vec![],
                aggregates: vec![AggregateColumn {
                    func: AggFunc::Count,
                    field: None,
                    alias: "n".into(),
                    cast_as: None,
                    inner_expr: None,
                }],
                filters: vec![],
                group_by: vec![],
                date_buckets: vec![],
                having: vec![],
                order_by: vec![],
                limit: None,
                offset: 0,
            },
            backend,
        ),
    );
}

fn render_upsert(out: &mut Out, backend: Backend, b: &str) {
    const U: &str = "upsert";
    out.stmt(
        U,
        "upsert",
        b,
        upsert::build_upsert("kv", &data(), &["id"], &["name", "count", "meta"], backend),
    );
    out.stmt(
        U,
        "upsert_do_nothing",
        b,
        upsert::build_upsert("kv", &data()[..2], &["id"], &[], backend),
    );
    out.stmt(
        U,
        "upsert_returning",
        b,
        upsert::build_upsert_returning("kv", &data()[..3], &["id", "name"], &["count"], backend),
    );
    out.stmt_result(
        U,
        "windowed_counter",
        b,
        upsert::build_windowed_counter_upsert(
            "rate_limits",
            "key",
            "rl1",
            "ip:1.2.3.4",
            "count",
            "window_start",
            &["created_at"],
            &["updated_at"],
            "2026-10-27T00:00:00Z",
            1_000,
            940,
            backend,
        ),
    );
    out.stmt_result(
        U,
        "windowed_counter_returning",
        b,
        upsert::build_windowed_counter_upsert_returning(
            "rate_limits",
            "key",
            "rl1",
            "ip:1.2.3.4",
            "count",
            "window_start",
            &[],
            &["updated_at"],
            "2026-10-27T00:00:00Z",
            1_000,
            940,
            backend,
        ),
    );
}

fn render_guard(out: &mut Out, backend: Backend, b: &str) {
    const G: &str = "guard";
    let guards = vec![
        CapGuard::CountBelow {
            filters: vec![f("org_id", FilterOp::Equal, json!("o1"))],
            cap: 10,
        },
        CapGuard::SumAtMost {
            field: "bytes".into(),
            filters: vec![
                f("org_id", FilterOp::Equal, json!("o1")),
                f("deleted_at", FilterOp::IsNull, Json::Null),
            ],
            add: 500,
            cap: 4_096,
        },
    ];
    out.stmt_result(
        G,
        "probe",
        b,
        guard::build_guard_probe("files", &guards, backend),
    );
    out.stmt_result(
        G,
        "insert_guarded",
        b,
        guard::build_insert_guarded("files", &data()[..3], &guards, backend),
    );
    out.stmt_result(
        G,
        "update_guarded",
        b,
        guard::build_update_guarded(
            "files",
            &data()[1..3],
            &[f("id", FilterOp::Equal, json!("r1"))],
            &guards,
            backend,
        ),
    );
    for (i, s) in guard::build_guard_preamble("files", backend)
        .into_iter()
        .enumerate()
    {
        out.stmt(G, &format!("preamble_{i}"), b, s);
    }
}

fn render_ddl(out: &mut Out, backend: Backend, b: &str) {
    const D: &str = "ddl";
    let mut owner = col_string("owner_id").not_null();
    owner.references = Some(wafer_schema::Reference {
        table: "users".into(),
        column: "id".into(),
        on_delete: "CASCADE".into(),
        on_update: "SET NULL".into(),
    });
    let table = wafer_schema::Table {
        name: "posts".into(),
        columns: {
            let mut cols = vec![
                pk("id"),
                owner,
                col_string("title").not_null(),
                col_int("views").def(default_int(0)),
                col_bool("draft"),
                col_json("meta"),
            ];
            cols.extend(timestamps());
            cols
        },
        indexes: vec![wafer_schema::Index {
            name: String::new(),
            columns: vec!["owner_id".into(), "title".into()],
            unique: true,
        }],
        primary_key: vec![],
        unique_keys: vec![],
    };
    out.stmt_result(
        D,
        "create_table",
        b,
        ddl::build_create_table(&table, backend),
    );
    out.stmt_result(
        D,
        "create_index",
        b,
        ddl::build_create_index("posts", &table.indexes[0], backend),
    );
    match ddl::build_fk_indexes(&table, backend) {
        Ok(stmts) => {
            for (i, s) in stmts.into_iter().enumerate() {
                out.stmt(D, &format!("fk_index_{i}"), b, s);
            }
        }
        Err(e) => {
            out.header(D, "fk_indexes", b);
            writeln!(out.0, "error: {e:?}\n").unwrap();
        }
    }
    out.stmt_result(
        D,
        "add_column",
        b,
        ddl::build_add_column("posts", &col_int("likes").def(default_int(0)), backend),
    );
    out.stmt_result(
        D,
        "add_column_for_value",
        b,
        ddl::build_add_column_for_value("posts", "score", &json!(1.5), backend),
    );
    out.stmt_result(D, "drop_table", b, ddl::build_drop_table("posts", backend));
}

fn render_introspect(out: &mut Out, backend: Backend, b: &str) {
    const I: &str = "introspect";
    out.raw(
        I,
        "list_tables",
        b,
        &introspect::build_list_tables(backend),
        &[],
    );
    let (sql, p) = introspect::build_list_tables_like("app__", backend);
    out.raw(I, "list_tables_like", b, &sql, &p);
    let (sql, p) = introspect::build_table_exists("users", backend);
    out.raw(I, "table_exists", b, &sql, &p);
    let (sql, p) = introspect::build_list_columns("users", backend);
    out.raw(I, "list_columns", b, &sql, &p);
    let (sql, p) = introspect::build_list_primary_key("users", backend);
    out.raw(I, "list_primary_key", b, &sql, &p);
    let (sql, p) = introspect::build_id_policy("users", backend);
    out.raw(I, "id_policy", b, &sql, &p);
    match introspect::build_table_info("users", backend) {
        Ok((sql, p)) => out.raw(I, "table_info", b, &sql, &p),
        Err(e) => {
            out.header(I, "table_info", b);
            writeln!(out.0, "error: {e:?}\n").unwrap();
        }
    }
    match introspect::build_table_row_count("users", backend) {
        Ok(sql) => out.raw(I, "table_row_count", b, &sql, &[]),
        Err(e) => {
            out.header(I, "table_row_count", b);
            writeln!(out.0, "error: {e:?}\n").unwrap();
        }
    }
}

/// The vector builders are SQLite-only (sqlite-vec / FTS5); rendered once.
fn render_vector(out: &mut Out) {
    const V: &str = "vector";
    const B: &str = "sqlite";
    let s = VectorIndexSchema::new("docs").expect("plain name");
    out.stmt(
        V,
        "create_vec_and_meta",
        B,
        s.build_create_vec_and_meta(384),
    );
    out.stmt(V, "create_fts", B, s.build_create_fts());
    for (i, st) in s.build_drop_all().into_iter().enumerate() {
        out.stmt(V, &format!("drop_all_{i}"), B, st);
    }
    out.stmt(V, "select_rowid_by_id", B, s.build_select_rowid_by_id());
    out.stmt(V, "insert_meta_autoinc", B, s.build_insert_meta_autoinc());
    out.stmt(V, "update_meta", B, s.build_update_meta());
    out.stmt(V, "count_meta", B, s.build_count_meta());
    out.stmt(V, "select_metadata_in", B, s.build_select_metadata_in(3));
    out.stmt(
        V,
        "select_ids_by_metadata",
        B,
        s.build_select_ids_by_metadata(2),
    );
    out.stmt(V, "select_rowid_in", B, s.build_select_rowid_in(2));
    out.stmt(V, "delete_meta_in", B, s.build_delete_meta_in(2));
    out.stmt(V, "insert_vec", B, s.build_insert_vec());
    out.stmt(V, "delete_vec_by_rowid", B, s.build_delete_vec_by_rowid());
    out.stmt(V, "insert_fts", B, s.build_insert_fts());
    out.stmt(V, "delete_fts_by_id", B, s.build_delete_fts_by_id());
    out.stmt(V, "delete_fts_in", B, s.build_delete_fts_in(2));
    out.stmt(V, "vec_knn_select", B, s.build_vec_knn_select());
    out.stmt(V, "fts_bm25_select", B, s.build_fts_bm25_select());
    out.stmt(
        V,
        "vec_knn_select_filtered",
        B,
        s.build_vec_knn_select_filtered(),
    );
    out.stmt(
        V,
        "fts_bm25_select_filtered",
        B,
        s.build_fts_bm25_select_filtered(),
    );
    let rename = VectorIndexRename::new("Docs", "docs").expect("plain names");
    for (i, st) in rename
        .build_statements("embedding float[384]", &["embedding".into()], true)
        .into_iter()
        .enumerate()
    {
        out.stmt(V, &format!("rename_{i}"), B, st);
    }
    let (a, c) = vector::build_list_meta_tables("app_");
    out.raw(V, "list_meta_tables", B, &format!("{a} | {c}"), &[]);
}

fn render_all() -> String {
    let mut out = Out(String::new());
    for (backend, b) in BACKENDS {
        render_query(&mut out, backend, b);
        render_aggregate(&mut out, backend, b);
        render_upsert(&mut out, backend, b);
        render_guard(&mut out, backend, b);
        render_ddl(&mut out, backend, b);
        render_introspect(&mut out, backend, b);
    }
    render_vector(&mut out);
    out.0
}

#[test]
fn generated_sql_matches_the_golden_file() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/golden/generated_sql.txt"
    );
    let actual = render_all();
    if std::env::var_os("WAFER_SQL_PIN_BLESS").is_some() {
        std::fs::write(path, &actual).expect("write golden file");
        return;
    }
    let expected = std::fs::read_to_string(path).expect("read golden file");
    if actual != expected {
        let first_diff = actual
            .lines()
            .zip(expected.lines())
            .enumerate()
            .find(|(_, (a, e))| a != e);
        match first_diff {
            Some((i, (a, e))) => panic!(
                "generated SQL drifted from {path} at line {}:\n  expected: {e}\n  actual:   {a}\n\
                 (re-bless with WAFER_SQL_PIN_BLESS=1 only if the change is intended)",
                i + 1
            ),
            None => panic!(
                "generated SQL drifted from {path}: line count {} vs expected {}",
                actual.lines().count(),
                expected.lines().count()
            ),
        }
    }
}
