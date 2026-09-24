//! The `database.aggregate` and `database.list` wire additions — aggregate
//! `cast_as`, `SumWhere`, column-to-column filters, and list pagination —
//! driven end to end:
//! request bytes encoded from a plain JSON-shaped value (what any peer puts on
//! the wire, independent of this build's Rust types) → the shared database
//! handler → the real SQLite service → the decoded response.
//!
//! The fixture's amounts live in REAL columns, so SQLite's uncast `SUM` reads
//! back as a JSON float — the same symptom Postgres produces for
//! `SUM(<bigint>)` (`NUMERIC`). That makes the cast observable on the backend
//! every CI run has (SQLite sums INTEGER, or integer-looking TEXT, to an
//! integer, so an integer column would pass with or without the cast); the
//! `BIGINT`-column Postgres proof is in the shared conformance suite.

use std::collections::HashMap;

use wafer_block::{
    codec,
    common::ServiceOp,
    context::Context,
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    types::{ResourceAccess, ResourceType},
    wire::database as wire,
    ErrorCode, Message, WaferError,
};
use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::interfaces::database::{
    handler::handle_message,
    service::{pk, Column, DataType, DatabaseService, Table},
};

/// A `Context` that grants every resource, so the requests reach the service.
struct AllowCtx;

#[wafer_block::wafer_async_trait]
impl Context for AllowCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        unimplemented!("the database handler makes no block calls")
    }
    fn is_cancelled(&self) -> bool {
        false
    }
    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }
    fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
        unimplemented!("the database handler does not clone its context")
    }
    fn check_resource_access(
        &self,
        _resource: &str,
        _resource_type: ResourceType,
        _access: ResourceAccess,
    ) -> Result<(), WaferError> {
        Ok(())
    }
    fn resource_access_admitted(
        &self,
        _resource: &str,
        _resource_type: ResourceType,
        _access: ResourceAccess,
    ) -> bool {
        true
    }
}

const TABLE: &str = "orders";

/// Three orders across two accounts, amounts in REAL columns (see the module
/// doc):
///   a: o1 total 1000 refunded    0
///      o2 total 2000 refunded 2500   (refunded > total)
///   b: o3 total 4000 refunded 4000   (refunded = total)
async fn seeded() -> SQLiteDatabaseService {
    let svc = SQLiteDatabaseService::open_in_memory().expect("open in-memory sqlite");
    svc.ensure_schema_table(&Table {
        name: TABLE.to_string(),
        columns: vec![
            pk("id"),
            Column::new("account", DataType::Text),
            Column::new("total_cents", DataType::Float),
            Column::new("refunded_cents", DataType::Float),
        ],
        indexes: Vec::new(),
        primary_key: Vec::new(),
        unique_keys: Vec::new(),
    })
    .await
    .expect("create orders");
    for (id, account, total, refunded) in [
        ("o1", "a", 1000, 0),
        ("o2", "a", 2000, 2500),
        ("o3", "b", 4000, 4000),
    ] {
        let data: HashMap<String, serde_json::Value> = [
            ("id", serde_json::json!(id)),
            ("account", serde_json::json!(account)),
            ("total_cents", serde_json::json!(total)),
            ("refunded_cents", serde_json::json!(refunded)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        svc.create(TABLE, data).await.expect("seed order");
    }
    svc
}

async fn dispatch(
    svc: &SQLiteDatabaseService,
    op: &str,
    request: &serde_json::Value,
) -> Result<Vec<u8>, WaferError> {
    let body = codec::encode(request).expect("encode request");
    match handle_message(svc, &AllowCtx, &Message::new(op), &body)
        .await
        .collect_buffered()
        .await
    {
        Ok(resp) => Ok(resp.body),
        Err(TerminalNotResponse::Error(e)) => Err(e),
        Err(_) => panic!("{op}: the handler ended the stream without a response or error"),
    }
}

async fn aggregate(
    svc: &SQLiteDatabaseService,
    aggregates: serde_json::Value,
) -> Result<Vec<wire::Record>, WaferError> {
    let request = serde_json::json!({
        "collection": TABLE,
        "select_columns": ["account"],
        "aggregates": aggregates,
        "group_by": [{ "Column": "account" }],
        "sort": [{ "field": "account" }],
    });
    let body = dispatch(svc, ServiceOp::DATABASE_AGGREGATE, &request).await?;
    Ok(codec::decode(&body).expect("decode aggregate rows"))
}

async fn expect_invalid(result: Result<impl std::fmt::Debug, WaferError>, what: &str) {
    let err = result.expect_err(&format!("{what}: must be rejected"));
    assert_eq!(
        err.code,
        ErrorCode::InvalidArgument,
        "{what}: expected INVALID_ARGUMENT, got {:?}: {}",
        err.code,
        err.message
    );
}

/// `data[key]` as an exact JSON integer; a float `3000.0` fails.
fn int(rec: &wire::Record, key: &str) -> i64 {
    rec.data
        .get(key)
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_else(|| {
            panic!(
                "{key:?} must be a JSON integer, got {:?}",
                rec.data.get(key)
            )
        })
}

#[tokio::test]
async fn sum_cast_as_bigint_returns_an_integer() {
    let svc = seeded().await;
    let rows = aggregate(
        &svc,
        serde_json::json!([
            { "Sum": { "field": "total_cents", "alias": "gross", "cast_as": "BIGINT" } },
            { "Sum": { "field": "total_cents", "alias": "gross_float" } },
        ]),
    )
    .await
    .expect("aggregate with a cast");
    assert_eq!(int(&rows[0], "gross"), 3000);
    assert_eq!(int(&rows[1], "gross"), 4000);
    // The uncast sum is the float the cast exists to avoid — if SQLite ever
    // returned an integer here, the cast assertion above would prove nothing.
    assert!(
        rows[0].data["gross_float"].is_f64(),
        "uncast SUM over REAL is REAL on SQLite: {:?}",
        rows[0].data["gross_float"]
    );
}

#[tokio::test]
async fn avg_cast_as_double_precision_returns_a_float() {
    let svc = seeded().await;
    let rows = aggregate(
        &svc,
        serde_json::json!([
            { "Avg": { "field": "total_cents", "alias": "mean", "cast_as": "double precision" } },
        ]),
    )
    .await
    .expect("aggregate with a lowercase cast name");
    assert_eq!(rows[0].data["mean"], serde_json::json!(1500.0));
}

#[tokio::test]
async fn cast_as_off_the_allowlist_is_invalid_argument() {
    let svc = seeded().await;
    for cast in [
        "INTEGER",
        "TEXT",
        "NUMERIC",
        "BIGINT) AS x FROM orders; DROP TABLE orders; --",
        "",
    ] {
        for agg in ["Sum", "Avg"] {
            let result = aggregate(
                &svc,
                serde_json::json!([
                    { agg: { "field": "total_cents", "alias": "x", "cast_as": cast } },
                ]),
            )
            .await;
            expect_invalid(result, &format!("{agg} cast_as {cast:?}")).await;
        }
        let result = aggregate(
            &svc,
            serde_json::json!([{ "SumWhere": {
                "field": "total_cents",
                "when": [{ "field": "account", "value": "a" }],
                "alias": "x",
                "cast_as": cast,
            } }]),
        )
        .await;
        expect_invalid(result, &format!("SumWhere cast_as {cast:?}")).await;
    }
    // The table survived the injection attempt, because it never ran.
    assert_eq!(svc.count(TABLE, &[]).await.expect("count"), 3);
}

#[tokio::test]
async fn sum_where_sums_the_field_over_matching_rows() {
    let svc = seeded().await;
    let rows = aggregate(
        &svc,
        serde_json::json!([
            { "SumWhere": {
                "field": "refunded_cents",
                "when": [{ "field": "refunded_cents", "operator": "gt", "value": 0 }],
                "alias": "refunded",
                "cast_as": "BIGINT",
            } },
            // Matches no row of account b: the inline `ELSE 0` makes it 0.
            { "SumWhere": {
                "field": "total_cents",
                "when": [{ "field": "total_cents", "operator": "lt", "value": 2000 }],
                "alias": "small",
                "cast_as": "BIGINT",
            } },
        ]),
    )
    .await
    .expect("aggregate with SumWhere");
    assert_eq!(int(&rows[0], "refunded"), 2500);
    assert_eq!(int(&rows[1], "refunded"), 4000);
    assert_eq!(int(&rows[0], "small"), 1000);
    assert_eq!(int(&rows[1], "small"), 0);
}

#[tokio::test]
async fn sum_where_rejects_an_empty_predicate_and_a_hostile_field() {
    let svc = seeded().await;
    let empty = aggregate(
        &svc,
        serde_json::json!([{ "SumWhere": { "field": "total_cents", "when": [], "alias": "x" } }]),
    )
    .await;
    expect_invalid(empty, "SumWhere with an empty `when`").await;
    let hostile = aggregate(
        &svc,
        serde_json::json!([{ "SumWhere": {
            "field": "total_cents\" FROM orders --",
            "when": [{ "field": "account", "value": "a" }],
            "alias": "x",
        } }]),
    )
    .await;
    expect_invalid(hostile, "SumWhere with a hostile field").await;
}

#[tokio::test]
async fn column_compare_in_a_conditional_aggregate() {
    let svc = seeded().await;
    let rows = aggregate(
        &svc,
        serde_json::json!([
            { "CaseWhenSum": {
                "when": [{ "field": "refunded_cents", "operator": "gt", "column": "total_cents" }],
                "alias": "over_refunded",
            } },
        ]),
    )
    .await
    .expect("aggregate with a column-to-column predicate");
    assert_eq!(int(&rows[0], "over_refunded"), 1, "o2 only");
    assert_eq!(
        int(&rows[1], "over_refunded"),
        0,
        "o3 is equal, not greater"
    );
}

async fn list_ids(
    svc: &SQLiteDatabaseService,
    filters: serde_json::Value,
) -> Result<Vec<String>, WaferError> {
    let request = serde_json::json!({
        "collection": TABLE,
        "filters": filters,
        "sort": [{ "field": "id" }],
    });
    let body = dispatch(svc, ServiceOp::DATABASE_LIST, &request).await?;
    let list: wire::RecordList = codec::decode(&body).expect("decode record list");
    assert_eq!(
        usize::try_from(list.total_count).expect("non-negative"),
        list.records.len(),
        "total_count must honour the same filters"
    );
    Ok(list.records.into_iter().map(|r| r.id).collect())
}

/// A list page as the wire carries it: `limit`/`offset` are whatever JSON the
/// peer sends, absent when `None`.
async fn list_page(
    svc: &SQLiteDatabaseService,
    page: serde_json::Value,
) -> Result<Vec<String>, WaferError> {
    let mut request = serde_json::json!({
        "collection": TABLE,
        "sort": [{ "field": "id" }],
    });
    request
        .as_object_mut()
        .expect("object")
        .extend(page.as_object().expect("page object").clone());
    let body = dispatch(svc, ServiceOp::DATABASE_LIST, &request).await?;
    let list: wire::RecordList = codec::decode(&body).expect("decode record list");
    Ok(list.records.into_iter().map(|r| r.id).collect())
}

#[tokio::test]
async fn list_pagination_over_the_wire() {
    let svc = seeded().await;
    assert_eq!(
        list_page(&svc, serde_json::json!({}))
            .await
            .expect("no limit"),
        ["o1", "o2", "o3"],
        "an absent limit returns every row"
    );
    assert_eq!(
        list_page(&svc, serde_json::json!({ "limit": 1, "offset": 1 }))
            .await
            .expect("one page"),
        ["o2"]
    );
    // SQLite cannot render OFFSET without LIMIT: refused, not a syntax error
    // surfacing as INTERNAL.
    expect_invalid(
        list_page(&svc, serde_json::json!({ "offset": 1 })).await,
        "an offset with no limit",
    )
    .await;
    // What an encoder from before `limit` became optional sends for "no
    // limit": refused, not an empty page.
    expect_invalid(
        list_page(&svc, serde_json::json!({ "limit": 0, "offset": 0 })).await,
        "a zero limit",
    )
    .await;
}

#[tokio::test]
async fn column_compare_in_a_list_filter() {
    let svc = seeded().await;
    // Every column operator, against the fixture's three rows.
    let cases = [
        ("eq", vec!["o3"]),
        ("neq", vec!["o1", "o2"]),
        ("gt", vec!["o2"]),
        ("gte", vec!["o2", "o3"]),
        ("lt", vec!["o1"]),
        ("lte", vec!["o1", "o3"]),
    ];
    for (op, want) in cases {
        let ids = list_ids(
            &svc,
            serde_json::json!([
                { "field": "refunded_cents", "operator": op, "column": "total_cents" },
            ]),
        )
        .await
        .unwrap_or_else(|e| panic!("list {op}: {e:?}"));
        assert_eq!(ids, want, "refunded_cents {op} total_cents");
    }
    // Inside an OR group, beside a value leaf.
    let ids = list_ids(
        &svc,
        serde_json::json!([{ "any": [
            { "field": "refunded_cents", "operator": "gt", "column": "total_cents" },
            { "field": "account", "value": "b" },
        ] }]),
    )
    .await
    .expect("list with a column compare in a group");
    assert_eq!(ids, vec!["o2", "o3"]);
}

#[tokio::test]
async fn malformed_column_compare_is_invalid_argument() {
    let svc = seeded().await;
    let cases = [
        (
            serde_json::json!({ "field": "refunded_cents", "operator": "gt", "column": "total_cents", "value": 0 }),
            "both value and column",
        ),
        (
            serde_json::json!({ "field": "account", "operator": "like", "column": "id" }),
            "like has no column form",
        ),
        (
            serde_json::json!({ "field": "account", "operator": "in", "column": "id" }),
            "in has no column form",
        ),
        (
            serde_json::json!({ "field": "account", "operator": "is_null", "column": "id" }),
            "is_null has no column form",
        ),
        (
            serde_json::json!({ "field": "account", "operator": "eq", "column": "id\" OR 1=1 --" }),
            "hostile column",
        ),
        (
            serde_json::json!({ "field": "account\" OR 1=1 --", "operator": "eq", "column": "id" }),
            "hostile field",
        ),
    ];
    for (leaf, what) in cases {
        expect_invalid(list_ids(&svc, serde_json::json!([leaf])).await, what).await;
    }
}

/// Every row, as `(id, data)` in id order — the whole table's state.
async fn snapshot(svc: &SQLiteDatabaseService) -> Vec<(String, String)> {
    let opts = wafer_block::db::ListOptions {
        sort: vec![wafer_block::db::SortField {
            field: "id".into(),
            desc: false,
        }],
        ..Default::default()
    };
    svc.list(TABLE, &opts)
        .await
        .expect("snapshot list")
        .records
        .into_iter()
        .map(|r| {
            let mut data: Vec<_> = r.data.into_iter().collect();
            data.sort_by(|a, b| a.0.cmp(&b.0));
            (r.id, format!("{data:?}"))
        })
        .collect()
}

/// A column-to-column leaf is a filter-TREE leaf only. Every op that takes
/// flat filters (`handler::flatten_leaves`) must refuse it as
/// `InvalidArgument` before any SQL runs — for a write op, running it with the
/// leaf dropped or misread would change rows the caller never selected. Each
/// request is otherwise valid (the control runs it with a value filter), and
/// the table is byte-identical afterwards.
#[tokio::test]
async fn flat_filter_ops_reject_column_compare() {
    let svc = seeded().await;
    let before = snapshot(&svc).await;
    let column_leaf = serde_json::json!([
        { "field": "refunded_cents", "operator": "gt", "column": "total_cents" },
    ]);
    // Matches no row, so the controls leave the table as it was too.
    let value_leaf = serde_json::json!([{ "field": "account", "value": "nobody" }]);
    let set = serde_json::json!({ "account": "rewritten" });
    let requests = |filters: &serde_json::Value| {
        [
            (
                ServiceOp::DATABASE_COUNT,
                serde_json::json!({ "collection": TABLE, "filters": filters }),
            ),
            (
                ServiceOp::DATABASE_SUM,
                serde_json::json!({ "collection": TABLE, "field": "total_cents", "filters": filters }),
            ),
            (
                ServiceOp::DATABASE_DELETE_WHERE,
                serde_json::json!({ "collection": TABLE, "filters": filters }),
            ),
            (
                ServiceOp::DATABASE_DELETE_WHERE_COUNT,
                serde_json::json!({ "collection": TABLE, "filters": filters }),
            ),
            (
                ServiceOp::DATABASE_TAKE_WHERE,
                serde_json::json!({ "collection": TABLE, "filters": filters }),
            ),
            (
                ServiceOp::DATABASE_UPDATE_WHERE,
                serde_json::json!({ "collection": TABLE, "filters": filters, "data": set }),
            ),
            (
                ServiceOp::DATABASE_UPDATE_WHERE_COUNT,
                serde_json::json!({ "collection": TABLE, "filters": filters, "data": set }),
            ),
            (
                ServiceOp::DATABASE_INCREMENT_FIELD_WHERE,
                serde_json::json!({
                    "collection": TABLE, "col": "total_cents", "delta": 1, "filters": filters,
                }),
            ),
            (
                ServiceOp::DATABASE_AGGREGATE,
                serde_json::json!({
                    "collection": TABLE,
                    "aggregates": [{ "Count": { "alias": "n" } }],
                    "filters": filters,
                }),
            ),
        ]
    };
    for (op, request) in requests(&value_leaf) {
        dispatch(&svc, op, &request)
            .await
            .unwrap_or_else(|e| panic!("control: {op} with a value filter is valid: {e:?}"));
    }
    for (op, request) in requests(&column_leaf) {
        expect_invalid(dispatch(&svc, op, &request).await, op).await;
    }
    assert_eq!(
        snapshot(&svc).await,
        before,
        "every rejection happened before any SQL ran"
    );
}

#[tokio::test]
async fn avg_cast_as_bigint_is_invalid_argument() {
    let svc = seeded().await;
    for cast in ["BIGINT", "bigint"] {
        let result = aggregate(
            &svc,
            serde_json::json!([
                { "Avg": { "field": "total_cents", "alias": "x", "cast_as": cast } },
            ]),
        )
        .await;
        expect_invalid(result, &format!("Avg cast_as {cast:?}")).await;
    }
    // The same cast stays valid on a sum, whose value is integral.
    aggregate(
        &svc,
        serde_json::json!([
            { "Sum": { "field": "total_cents", "alias": "x", "cast_as": "BIGINT" } },
        ]),
    )
    .await
    .expect("Sum cast_as BIGINT");
}

/// Ungrouped over no rows, `SUM` is `NULL`; the conditional sum and the
/// conditional count answer 0.
#[tokio::test]
async fn conditional_sums_over_no_rows_are_zero() {
    let svc = seeded().await;
    let request = serde_json::json!({
        "collection": TABLE,
        "aggregates": [
            { "SumWhere": {
                "field": "total_cents",
                "when": [{ "field": "account", "value": "a" }],
                "alias": "summed",
                "cast_as": "BIGINT",
            } },
            { "CaseWhenSum": {
                "when": [{ "field": "account", "value": "a" }],
                "alias": "counted",
            } },
        ],
        "filters": [{ "field": "account", "value": "nobody" }],
    });
    let body = dispatch(&svc, ServiceOp::DATABASE_AGGREGATE, &request)
        .await
        .expect("ungrouped aggregate over no rows");
    let rows: Vec<wire::Record> = codec::decode(&body).expect("decode aggregate rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(int(&rows[0], "summed"), 0);
    assert_eq!(int(&rows[0], "counted"), 0);
}
