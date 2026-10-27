//! How the walkers over a JSON Schema document read it: which keywords hold
//! subschemas ([`keyword_value`]), and the encoding between a `$defs` key and
//! the `#/$defs/` pointer segment that names it ([`encode_ref_name`],
//! [`decode_ref_name`]).
//!
//! Shared by the two places that walk endpoint schemas and write or follow
//! their pointers: the endpoint schema derivation
//! (`endpoint::self_contained_schema`, which turns repeated copies of a
//! recursive type into `#/$defs/X` references) and `wafer-core`'s discovery
//! projections (which follow those references for WebMCP and rewrite them
//! for OpenAPI). One owner, so a key written by one side is always found by
//! the other, and a keyword one side reads as a schema position is one the
//! other reads the same way.

/// What the value of a keyword is, read as a member of a *schema object*
/// (an object whose keys are JSON Schema keywords) — see [`keyword_value`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeywordValue {
    /// A single subschema (`items`, `not`, `if`, ...).
    Subschema,
    /// An array of subschemas (`allOf`, `anyOf`, ...).
    SubschemaList,
    /// A map from names the author chose to subschemas (`properties`, ...).
    ///
    /// The map's keys are *not* keywords. `struct S { default: Status }`
    /// emits `{"properties": {"default": {"$ref": "#/$defs/Status"}}}`, and
    /// that `default` is a field name whose value is a schema like any other,
    /// so a walk has to know it is inside such a map rather than guess from
    /// the key.
    SubschemaMap,
    /// Instance data that a validator compares the instance against, never a
    /// schema (`default`, `const`, ...).
    ///
    /// A walk must copy it verbatim. `{"$ref": "https://example.com/x"}` or
    /// `{"$ref": "#/$defs/D", "note": "..."}` sitting in a `default` is legal
    /// user data — an object with a key that happens to be spelled `$ref` —
    /// and reading it as a reference would either report a broken schema or
    /// silently rewrite a default value the endpoint declared. Likewise a
    /// literal may have a `$defs` key, which is data, not a reference table.
    Literal,
    /// Every other keyword: one whose value is not a subschema in draft
    /// 2020-12 (`type`, `required`, `minimum`, ...), the reference keywords
    /// `$ref` and `$defs` (each walk decides for itself how it treats the
    /// reference and the table), and any keyword this classification does
    /// not know (draft-07's `definitions` or `additionalItems`, an `x-`
    /// extension).
    ///
    /// What a walk does with one depends on which way it is safe for that
    /// walk to err, so it is the caller's decision: the endpoint schema
    /// derivation leaves it alone (a missed rewrite there only leaves a copy
    /// of a definition inlined), while `wafer-core`'s reference resolution
    /// walks it as a schema (an unwalked `$ref` there would dangle).
    Other,
}

/// Classify `keyword` as a member of a schema object — the one list of
/// which JSON Schema 2020-12 keywords hold subschemas, and in what shape.
///
/// The answer only holds where the surrounding object's keys are read as
/// keywords. The members of a [`KeywordValue::SubschemaMap`] value are named
/// by the author, and a member named `default` or `items` is still a schema.
pub fn keyword_value(keyword: &str) -> KeywordValue {
    if SUBSCHEMA_KEYWORDS.contains(&keyword) {
        KeywordValue::Subschema
    } else if SUBSCHEMA_LIST_KEYWORDS.contains(&keyword) {
        KeywordValue::SubschemaList
    } else if SUBSCHEMA_MAP_KEYWORDS.contains(&keyword) {
        KeywordValue::SubschemaMap
    } else if LITERAL_VALUE_KEYWORDS.contains(&keyword) {
        KeywordValue::Literal
    } else {
        KeywordValue::Other
    }
}

/// The [`KeywordValue::Subschema`] keywords.
const SUBSCHEMA_KEYWORDS: &[&str] = &[
    "items",
    "additionalProperties",
    "not",
    "if",
    "then",
    "else",
    "contains",
    "propertyNames",
    "unevaluatedItems",
    "unevaluatedProperties",
    "contentSchema",
];

/// The [`KeywordValue::SubschemaList`] keywords.
const SUBSCHEMA_LIST_KEYWORDS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems"];

/// The [`KeywordValue::SubschemaMap`] keywords.
const SUBSCHEMA_MAP_KEYWORDS: &[&str] = &["properties", "patternProperties", "dependentSchemas"];

/// The [`KeywordValue::Literal`] keywords.
const LITERAL_VALUE_KEYWORDS: &[&str] = &["default", "const", "enum", "examples"];

/// Decode a `#/$defs/` pointer segment back into the key it names in the
/// `$defs` table.
///
/// schemars writes reference *names* through its `encode_ref_name`: `~`
/// becomes `~0` and `/` becomes `~1` (RFC 6901 JSON-Pointer escaping), and
/// every other byte outside the URI-fragment safe set is percent-encoded
/// (space, `"`, `#`, `%`, `<`, `>`, `[`, `\`, `]`, `^`, `` ` ``, `{`, `|`,
/// `}`, and anything non-ASCII). The `$defs` *keys* are left unencoded, so
/// `#[schemars(rename = "Product Status")]` emits a reference to
/// `#/$defs/Product%20Status` against a table keyed `Product Status`.
/// Looking the raw segment up would miss and silently degrade the property
/// to `{}`.
///
/// Unescaping order is load-bearing, and is the order RFC 6901 §4 requires:
/// `~1` first, then `~0`. Doing `~0` first would turn the encoding of the
/// literal name `~1` (which is `~01`) into `~1` and then into `/`.
///
/// Returns `None` when percent-decoding does not yield valid UTF-8 — a
/// segment that cannot name any key, and so must be reported as unresolvable
/// rather than guessed at.
pub fn decode_ref_name(encoded: &str) -> Option<String> {
    let decoded = percent_encoding::percent_decode_str(encoded)
        .decode_utf8()
        .ok()?;
    Some(decoded.replace("~1", "/").replace("~0", "~"))
}

/// The bytes a `#/$defs/` pointer segment may carry unencoded: the RFC 3986
/// unreserved set (`ALPHA / DIGIT / "-" / "." / "_" / "~"`). Everything else
/// is percent-encoded, including every non-ASCII byte —
/// `percent_encoding` escapes those regardless of the set.
const REF_NAME_ESCAPE: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Encode a `$defs` key into the pointer segment that names it — the exact
/// inverse of [`decode_ref_name`].
///
/// The `$defs` keys stay the *unencoded* names, exactly as schemars leaves
/// them, so a definition named `Product Status` is keyed `Product Status`
/// and referred to as `#/$defs/Product%20Status`. schemars itself leaves a
/// few more sub-delimiters (`!`, `$`, `(`, `)`, ...) unencoded than this
/// does; both forms decode to the same key.
///
/// Order is load-bearing and mirrors the decoder's. JSON-Pointer escaping
/// comes first (`~` → `~0`, then `/` → `~1`, in that order, so the `~` that
/// `~1` introduces is not escaped a second time), then percent-encoding —
/// which leaves the escapes alone, since `~`, `0` and `1` are all unreserved.
pub fn encode_ref_name(name: &str) -> String {
    let escaped = name.replace('~', "~0").replace('/', "~1");
    percent_encoding::utf8_percent_encode(&escaped, REF_NAME_ESCAPE).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classification is the one list both walkers read, so a change to
    /// it changes both at once — pinned here so that change is deliberate.
    #[test]
    fn keyword_classification_is_pinned() {
        assert_eq!(
            SUBSCHEMA_KEYWORDS,
            [
                "items",
                "additionalProperties",
                "not",
                "if",
                "then",
                "else",
                "contains",
                "propertyNames",
                "unevaluatedItems",
                "unevaluatedProperties",
                "contentSchema",
            ]
        );
        assert_eq!(
            SUBSCHEMA_LIST_KEYWORDS,
            ["allOf", "anyOf", "oneOf", "prefixItems"]
        );
        assert_eq!(
            SUBSCHEMA_MAP_KEYWORDS,
            ["properties", "patternProperties", "dependentSchemas"]
        );
        assert_eq!(
            LITERAL_VALUE_KEYWORDS,
            ["default", "const", "enum", "examples"]
        );

        // Each list is one variant, and no keyword sits in two.
        for (list, value) in [
            (SUBSCHEMA_KEYWORDS, KeywordValue::Subschema),
            (SUBSCHEMA_LIST_KEYWORDS, KeywordValue::SubschemaList),
            (SUBSCHEMA_MAP_KEYWORDS, KeywordValue::SubschemaMap),
            (LITERAL_VALUE_KEYWORDS, KeywordValue::Literal),
        ] {
            for keyword in list {
                assert_eq!(keyword_value(keyword), value, "{keyword}");
            }
        }
        for other in [
            "$ref",
            "$defs",
            "type",
            "required",
            "title",
            "description",
            "minimum",
            "definitions",
            "additionalItems",
            "dependencies",
            "x-extension",
        ] {
            assert_eq!(keyword_value(other), KeywordValue::Other, "{other}");
        }
    }

    #[test]
    fn encoding_round_trips_awkward_names() {
        for name in ["Condition", "Product Status", "a/b", "~1", "x~y/z", "Grüße"] {
            let encoded = encode_ref_name(name);
            assert_eq!(
                decode_ref_name(&encoded).as_deref(),
                Some(name),
                "{encoded}"
            );
        }
        assert_eq!(encode_ref_name("Product Status"), "Product%20Status");
        assert_eq!(encode_ref_name("~1"), "~01");
    }
}
