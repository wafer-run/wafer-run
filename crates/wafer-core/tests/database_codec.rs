//! The shared SQL row codec
//! ([`wafer_core::interfaces::database::codec`]) — the one decode policy every
//! SQL-family backend applies to a result row.
//!
//! A text value is JSON to parse only in a column the schema declares JSON,
//! never because of what the text looks like: a user's title `[1]` or `{}`
//! is a string. These tests pin the policy; the backend-agnostic half (that a
//! live service decodes by the table's declared types) is pinned by
//! `conformance::run_conformance`.

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
// record_from_json_row
// ---------------------------------------------------------------------------

#[test]
fn record_from_json_row_splits_out_the_id_and_keeps_it_in_data() {
    let rec = codec::record_from_json_row(
        serde_json::json!({
            "id": "r1",
            "name": "alpha",
        }),
        JsonColumns::NONE,
    );
    assert_eq!(rec.id, "r1");
    assert_eq!(rec.data.get("id"), Some(&serde_json::json!("r1")));
    assert_eq!(rec.data.get("name"), Some(&serde_json::json!("alpha")));
}

#[test]
fn record_from_json_row_stringifies_a_numeric_id() {
    let rec = codec::record_from_json_row(serde_json::json!({ "id": 7 }), JsonColumns::NONE);
    assert_eq!(rec.id, "7");
    assert_eq!(rec.data.get("id"), Some(&serde_json::json!(7)));
}

#[test]
fn record_from_json_row_parses_only_the_json_columns() {
    // A row as D1 and sql.js hand it over: every text column is a string.
    let rec = codec::record_from_json_row(
        serde_json::json!({
            "id": "[1]",
            "payload": r#"{"k":[1,2]}"#,
            "tags": "[\"a\"]",
            "title": "[1]",
            "note": "{}",
        }),
        &json_columns(&["payload", "tags"]),
    );
    assert_eq!(
        rec.data.get("payload"),
        Some(&serde_json::json!({"k":[1,2]}))
    );
    assert_eq!(rec.data.get("tags"), Some(&serde_json::json!(["a"])));
    assert_eq!(rec.data.get("title"), Some(&serde_json::json!("[1]")));
    assert_eq!(rec.data.get("note"), Some(&serde_json::json!("{}")));
    assert_eq!(rec.id, "[1]", "a JSON-looking id is still the id");
}

#[test]
fn record_from_json_row_on_a_non_object_is_an_empty_record() {
    let rec = codec::record_from_json_row(serde_json::json!(5), JsonColumns::NONE);
    assert_eq!(rec.id, "");
    assert!(rec.data.is_empty());
}

#[test]
fn record_from_json_row_leaves_non_string_columns_alone() {
    let rec = codec::record_from_json_row(
        serde_json::json!({
            "id": "r1",
            "n": 3,
            "f": 1.5,
            "b": true,
            "nil": serde_json::Value::Null,
            "obj": {"already": "structured"},
        }),
        &json_columns(&["n", "f", "b", "nil", "obj"]),
    );
    assert_eq!(rec.data.get("n"), Some(&serde_json::json!(3)));
    assert_eq!(rec.data.get("f"), Some(&serde_json::json!(1.5)));
    assert_eq!(rec.data.get("b"), Some(&serde_json::json!(true)));
    assert_eq!(rec.data.get("nil"), Some(&serde_json::Value::Null));
    assert_eq!(
        rec.data.get("obj"),
        Some(&serde_json::json!({"already": "structured"}))
    );
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
