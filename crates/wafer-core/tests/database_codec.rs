//! The shared SQL row codec
//! ([`wafer_core::interfaces::database::codec`]) — the one decode policy every
//! SQL-family backend applies to a result row.
//!
//! A text value is JSON to parse only in a column the schema declares JSON,
//! never because of what the text looks like: a user's title `[1]` or `{}`
//! is a string. These tests pin the policy; the backend-agnostic half (that a
//! live service decodes by the table's declared types) is pinned by
//! `conformance::run_conformance`.

use serde_json::json;
use wafer_core::interfaces::database::codec::{self, JsonColumns};

fn json_columns(names: &[&str]) -> JsonColumns {
    JsonColumns::new(names.iter().copied())
}

// ---------------------------------------------------------------------------
// decode_text
// ---------------------------------------------------------------------------

#[test]
fn decode_text_parses_a_json_columns_text() {
    let json = json_columns(&["meta"]);
    for (text, want) in [
        (r#"{"a":1}"#, serde_json::json!({"a": 1})),
        ("[1,2]", serde_json::json!([1, 2])),
        ("{}", serde_json::json!({})),
        ("[]", serde_json::json!([])),
        ("42", serde_json::json!(42)),
        (r#""quoted""#, serde_json::json!("quoted")),
    ] {
        assert_eq!(codec::decode_text("meta", text, &json), want, "{text:?}");
    }
}

#[test]
fn decode_text_keeps_every_other_columns_text_as_a_string() {
    let json = json_columns(&["meta"]);
    for text in [
        r#"{"a":1}"#,
        "[1]",
        "{}",
        "[]",
        "42",
        "true",
        "null",
        "plain",
    ] {
        assert_eq!(
            codec::decode_text("title", text, &json),
            serde_json::Value::String(text.to_string()),
            "{text:?} in a text column must stay a string"
        );
        assert_eq!(
            codec::decode_text("meta", text, JsonColumns::NONE),
            serde_json::Value::String(text.to_string()),
            "with no JSON columns, {text:?} must stay a string"
        );
    }
}

#[test]
fn decode_text_keeps_a_json_columns_unparsable_text_as_a_string() {
    // A value written before the column was declared JSON is returned
    // verbatim rather than lost.
    let json = json_columns(&["meta"]);
    for text in [r#"{"a":}"#, "{not json}", "[1,", "hello", ""] {
        assert_eq!(
            codec::decode_text("meta", text, &json),
            serde_json::Value::String(text.to_string()),
            "{text:?} is not valid JSON and must stay a string"
        );
    }
}

#[test]
fn json_column_names_match_case_insensitively() {
    let json = json_columns(&["meta"]);
    assert!(json.contains("META"));
    assert!(json.contains("Meta"));
    assert!(!json.contains("metadata"));
    assert!(JsonColumns::NONE.is_empty());
}

// ---------------------------------------------------------------------------
// record_from_columns
// ---------------------------------------------------------------------------

/// A row as a JS-value backend hands it over: name → value pairs in result
/// column order.
fn row(pairs: &[(&str, serde_json::Value)]) -> Vec<(String, serde_json::Value)> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), value.clone()))
        .collect()
}

#[test]
fn record_from_columns_splits_out_the_id_and_keeps_it_in_data() {
    let rec = codec::record_from_columns(
        row(&[("id", json!("r1")), ("name", json!("alpha"))]),
        JsonColumns::NONE,
    );
    assert_eq!(rec.id, "r1");
    assert_eq!(rec.data.get("id"), Some(&json!("r1")));
    assert_eq!(rec.data.get("name"), Some(&json!("alpha")));
}

#[test]
fn record_from_columns_keeps_the_result_column_order() {
    // `SELECT b, a, id, c`: not name order, not insertion into a hash map.
    let rec = codec::record_from_columns(
        row(&[
            ("b", json!(2)),
            ("a", json!("[1]")),
            ("id", json!("r1")),
            ("c", json!(null)),
        ]),
        &json_columns(&["a"]),
    );
    let names: Vec<&str> = rec.data.keys().map(String::as_str).collect();
    assert_eq!(names, ["b", "a", "id", "c"]);
    assert_eq!(
        rec.data.get("a"),
        Some(&json!([1])),
        "still decoded in place"
    );
}

#[test]
fn record_from_columns_stringifies_a_numeric_id() {
    let rec = codec::record_from_columns(row(&[("id", json!(7))]), JsonColumns::NONE);
    assert_eq!(rec.id, "7");
    assert_eq!(rec.data.get("id"), Some(&json!(7)));
}

#[test]
fn record_from_columns_parses_only_the_json_columns() {
    // A row as D1 and sql.js hand it over: every text column is a string.
    let rec = codec::record_from_columns(
        row(&[
            ("id", json!("[1]")),
            ("payload", json!(r#"{"k":[1,2]}"#)),
            ("tags", json!("[\"a\"]")),
            ("title", json!("[1]")),
            ("note", json!("{}")),
        ]),
        &json_columns(&["payload", "tags"]),
    );
    assert_eq!(rec.data.get("payload"), Some(&json!({"k":[1,2]})));
    assert_eq!(rec.data.get("tags"), Some(&json!(["a"])));
    assert_eq!(rec.data.get("title"), Some(&json!("[1]")));
    assert_eq!(rec.data.get("note"), Some(&json!("{}")));
    assert_eq!(rec.id, "[1]", "a JSON-looking id is still the id");
}

#[test]
fn record_from_columns_on_no_columns_is_an_empty_record() {
    let rec = codec::record_from_columns(Vec::new(), JsonColumns::NONE);
    assert_eq!(rec.id, "");
    assert!(rec.data.is_empty());
}

#[test]
fn record_from_columns_leaves_non_string_columns_alone() {
    let rec = codec::record_from_columns(
        row(&[
            ("id", json!("r1")),
            ("n", json!(3)),
            ("f", json!(1.5)),
            ("b", json!(true)),
            ("nil", serde_json::Value::Null),
            ("obj", json!({"already": "structured"})),
        ]),
        &json_columns(&["n", "f", "b", "nil", "obj"]),
    );
    assert_eq!(rec.data.get("n"), Some(&json!(3)));
    assert_eq!(rec.data.get("f"), Some(&json!(1.5)));
    assert_eq!(rec.data.get("b"), Some(&json!(true)));
    assert_eq!(rec.data.get("nil"), Some(&serde_json::Value::Null));
    assert_eq!(rec.data.get("obj"), Some(&json!({"already": "structured"})));
}

// ---------------------------------------------------------------------------
// scalar extraction
// ---------------------------------------------------------------------------

#[test]
fn first_scalar_takes_the_only_column_whatever_its_alias() {
    assert_eq!(
        codec::first_scalar(serde_json::json!({ "COUNT(*)": 3 })),
        Some(serde_json::json!(3))
    );
    assert_eq!(
        codec::first_scalar(serde_json::json!(3)),
        Some(serde_json::json!(3))
    );
    assert_eq!(codec::first_scalar(serde_json::json!({})), None);
}

#[test]
fn scalar_i64_accepts_either_numeric_shape_and_defaults_to_zero() {
    assert_eq!(codec::scalar_i64(Some(serde_json::json!({ "c": 7 }))), 7);
    assert_eq!(codec::scalar_i64(Some(serde_json::json!({ "c": 7.9 }))), 7);
    assert_eq!(codec::scalar_i64(Some(serde_json::json!({ "c": "x" }))), 0);
    assert_eq!(codec::scalar_i64(None), 0);
}

#[test]
fn scalar_f64_accepts_either_numeric_shape_and_defaults_to_zero() {
    assert!((codec::scalar_f64(Some(serde_json::json!({ "s": 2.5 }))) - 2.5).abs() < f64::EPSILON);
    assert!((codec::scalar_f64(Some(serde_json::json!({ "s": 2 }))) - 2.0).abs() < f64::EPSILON);
    assert!(codec::scalar_f64(Some(serde_json::json!({ "s": "x" }))).abs() < f64::EPSILON);
    assert!(codec::scalar_f64(None).abs() < f64::EPSILON);
}
