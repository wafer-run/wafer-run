//! The encoding between a `$defs` key and the `#/$defs/` pointer segment
//! that names it.
//!
//! Shared by the two places that write or follow such pointers: the endpoint
//! schema derivation (`endpoint::self_contained_schema`, which turns repeated
//! copies of a recursive type into `#/$defs/X` references) and `wafer-core`'s
//! discovery projections (which follow those references for WebMCP and
//! rewrite them for OpenAPI). One owner, so a key written by one side is
//! always found by the other.

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
