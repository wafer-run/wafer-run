//! Error types raised while converting block-manifest collection definitions
//! into runtime schema [`crate::Table`] types.

use thiserror::Error;

/// A problem found while translating a [`crate::manifest::CollectionDef`] into a
/// runtime schema [`crate::Table`].
///
/// These represent authoring mistakes in a block manifest (an unknown column
/// type, a malformed foreign-key reference). They are surfaced rather than
/// silently coerced so the mistake is visible at materialisation time.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SchemaError {
    /// A field declared a `type` that is not one of the supported logical
    /// column types. Previously such values were silently coerced to `STRING`.
    #[error(
        "collection '{collection}' field '{field}' has unknown type '{ty}' \
         (supported: string, text, int, integer, int64, float, number, bool, \
         boolean, datetime, json, blob)"
    )]
    UnknownFieldType {
        /// The collection (table) that contains the offending field.
        collection: String,
        /// The field (column) with the unknown type.
        field: String,
        /// The unrecognised type string from the manifest.
        ty: String,
    },

    /// A field declared a non-empty `ref` (foreign key) that does not contain a
    /// `.` separator, so it cannot be split into `table.column`. Previously such
    /// references were silently dropped, producing a column with no foreign key.
    #[error(
        "collection '{collection}' field '{field}' has malformed ref '{reference}' \
         (expected 'table.column')"
    )]
    BadRef {
        /// The collection (table) that contains the offending field.
        collection: String,
        /// The field (column) with the malformed reference.
        field: String,
        /// The malformed reference string from the manifest.
        reference: String,
    },

    /// A field declared a numeric `default` that does not fit the column's
    /// declared type — e.g. an integer column with a default larger than
    /// `i64::MAX` or a fractional value, or a number serde cannot represent as
    /// `f64`. Previously such defaults were silently coerced to `0` / `0.0`,
    /// installing a default the author never asked for.
    #[error(
        "collection '{collection}' field '{field}' has out-of-range numeric \
         default '{value}' for its declared type"
    )]
    BadDefault {
        /// The collection (table) that contains the offending field.
        collection: String,
        /// The field (column) with the out-of-range default.
        field: String,
        /// The offending default value, rendered as JSON.
        value: String,
    },
}
