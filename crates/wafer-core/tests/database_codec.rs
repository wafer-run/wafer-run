//! The shared SQL row codec
//! ([`wafer_core::interfaces::database::codec`]) — the one decode policy every
//! SQL-family backend applies to a result row.
//!
//! Before this module existed each backend carried its own copy and they had
//! drifted: native SQLite re-parsed a JSON-looking TEXT column back into a
//! structured value, the browser (sql.js) adapter did the same, and the
//! Cloudflare D1 adapter did not — so the *same* row read through the *same*
//! block code came back as `Value::Object` on two platforms and
//! `Value::String` on the third. These tests pin the single policy; the
//! backend-agnostic half (that a live service actually applies it) is pinned by
//! `conformance::run_conformance`.

use wafer_core::interfaces::database::codec;

// ---------------------------------------------------------------------------
// decode_text_value
// ---------------------------------------------------------------------------

#[test]
fn decode_text_value_parses_json_objects_and_arrays() {
    assert_eq!(
        codec::decode_text_value(r#"{"a":1}"#),
        serde_json::json!({"a": 1})
    );
    assert_eq!(codec::decode_text_value("[1,2]"), serde_json::json!([1, 2]));
    assert_eq!(codec::decode_text_value("{}"), serde_json::json!({}));
    assert_eq!(codec::decode_text_value("[]"), serde_json::json!([]));
}

#[test]
fn decode_text_value_keeps_plain_text_as_a_string() {
    for text in ["", "hello", "2026-01-15T00:00:00Z", "42", "true", "null"] {
        assert_eq!(
            codec::decode_text_value(text),
            serde_json::Value::String(text.to_string()),
            "{text:?} must decode as a string, not a JSON scalar"
        );
    }
}

#[test]
fn decode_text_value_keeps_malformed_json_as_a_string() {
    // Braced/bracketed but not valid JSON: the value is returned verbatim
    // rather than lost, so a column holding hand-written text that happens to
    // start with `{` survives the round trip.
    for text in [r#"{"a":}"#, "{not json}", "[1,", "[1,2", "{a:1}"] {
        assert_eq!(
            codec::decode_text_value(text),
            serde_json::Value::String(text.to_string()),
            "{text:?} is not valid JSON and must stay a string"
        );
    }
}

#[test]
fn decode_text_value_requires_the_braces_to_bound_the_whole_value() {
    // The predicate is deliberately "starts with `{` and ends with `}`" on the
    // raw text, with no trimming: leading/trailing whitespace means the column
    // is not a serialized JSON value this codec produced.
    for text in [" {\"a\":1}", "{\"a\":1} ", "x{\"a\":1}", "{\"a\":1}x"] {
        assert_eq!(
            codec::decode_text_value(text),
            serde_json::Value::String(text.to_string()),
            "{text:?} must stay a string"
        );
    }
}

// ---------------------------------------------------------------------------
// record_from_json_row
// ---------------------------------------------------------------------------

#[test]
fn record_from_json_row_splits_out_the_id_and_keeps_it_in_data() {
    let rec = codec::record_from_json_row(serde_json::json!({
        "id": "r1",
        "name": "alpha",
    }));
    assert_eq!(rec.id, "r1");
    assert_eq!(rec.data.get("id"), Some(&serde_json::json!("r1")));
    assert_eq!(rec.data.get("name"), Some(&serde_json::json!("alpha")));
}

#[test]
fn record_from_json_row_stringifies_a_numeric_id() {
    let rec = codec::record_from_json_row(serde_json::json!({ "id": 7 }));
    assert_eq!(rec.id, "7");
    assert_eq!(rec.data.get("id"), Some(&serde_json::json!(7)));
}

#[test]
fn record_from_json_row_reparses_json_looking_text_columns() {
    // This is the B25 divergence: the D1 adapter produced `Value::String` here
    // while SQLite and the browser produced `Value::Object`.
    let rec = codec::record_from_json_row(serde_json::json!({
        "id": "r1",
        "payload": r#"{"k":[1,2]}"#,
        "tags": "[\"a\"]",
        "note": "plain",
    }));
    assert_eq!(
        rec.data.get("payload"),
        Some(&serde_json::json!({"k":[1,2]}))
    );
    assert_eq!(rec.data.get("tags"), Some(&serde_json::json!(["a"])));
    assert_eq!(rec.data.get("note"), Some(&serde_json::json!("plain")));
}

#[test]
fn record_from_json_row_reparses_an_id_column_that_holds_json() {
    // A re-parsed `id` is no longer a string or a number, so there is no
    // sensible `Record::id` — it is empty rather than the raw text, matching
    // every backend's existing "id is a string or a number" rule.
    let rec = codec::record_from_json_row(serde_json::json!({ "id": "{}" }));
    assert_eq!(rec.id, "");
    assert_eq!(rec.data.get("id"), Some(&serde_json::json!({})));
}

#[test]
fn record_from_json_row_on_a_non_object_is_an_empty_record() {
    let rec = codec::record_from_json_row(serde_json::json!(5));
    assert_eq!(rec.id, "");
    assert!(rec.data.is_empty());
}

#[test]
fn record_from_json_row_leaves_non_string_columns_alone() {
    let rec = codec::record_from_json_row(serde_json::json!({
        "id": "r1",
        "n": 3,
        "f": 1.5,
        "b": true,
        "nil": serde_json::Value::Null,
        "obj": {"already": "structured"},
    }));
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
