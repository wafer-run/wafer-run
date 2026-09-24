use sea_query::Iden;
use wafer_block::db::is_plain_ident;

use crate::SqlBuildError;

/// Runtime identifier that implements sea_query::Iden.
/// Used for table and column names known only at runtime.
#[derive(Debug, Clone)]
pub struct DynCol(pub String);

impl Iden for DynCol {
    fn unquoted(&self, s: &mut dyn std::fmt::Write) {
        write!(s, "{}", self.0).unwrap();
    }
}

/// Validate that `name` is a plain identifier — non-empty, at most
/// [`MAX_IDENT_LEN`](wafer_block::db::MAX_IDENT_LEN) bytes, ASCII lowercase letters, digits and `_` only
/// ([`wafer_block::db::is_plain_ident`]) — and return it unchanged.
///
/// This is the fail-closed guard for identifiers that have to be spliced
/// into raw SQL text rather than quoted or parameter-bound (index names,
/// `PRAGMA` arguments, vector table names, raw expression columns), for every
/// name a DDL builder quotes, and for the table and column names a caller
/// hands the shared SQL executor. It is the same rule the database handler
/// applies to the wire, so a caller of the executor cannot get past it by
/// skipping the handler. Anything outside the allowed set is rejected with
/// [`SqlBuildError::InvalidIdentifier`] instead of being rewritten:
/// character-stripping can turn one valid identifier into a *different*
/// valid identifier (`"users; DROP TABLE"` → `"usersDROPTABLE"`, `a-b` →
/// `ab`), and PostgreSQL's truncation of a longer name would do the same.
pub fn validate_ident(name: &str) -> Result<&str, SqlBuildError> {
    if !is_plain_ident(name) {
        return Err(SqlBuildError::InvalidIdentifier {
            value: name.to_string(),
        });
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use wafer_block::db::MAX_IDENT_LEN;

    use super::*;

    #[test]
    fn test_validate_ident_accepts_plain_identifiers() {
        assert_eq!(validate_ident("users"), Ok("users"));
        assert_eq!(validate_ident("created_at"), Ok("created_at"));
        assert_eq!(validate_ident("table123"), Ok("table123"));
        assert_eq!(validate_ident("_private"), Ok("_private"));
    }

    #[test]
    fn test_validate_ident_rejects_injection_attempts() {
        for bad in [
            "users; DROP TABLE",
            "col\"name",
            "a-b",
            "a.b",
            "a b",
            "Table123",
        ] {
            assert_eq!(
                validate_ident(bad),
                Err(SqlBuildError::InvalidIdentifier {
                    value: bad.to_string()
                }),
                "expected rejection of {bad:?}"
            );
        }
    }

    #[test]
    fn test_validate_ident_rejects_empty() {
        assert_eq!(
            validate_ident(""),
            Err(SqlBuildError::InvalidIdentifier {
                value: String::new()
            })
        );
    }

    #[test]
    fn test_validate_ident_rejects_non_ascii_alphanumerics() {
        // `char::is_alphanumeric` admits `é`; the validator allows ASCII
        // only.
        assert_eq!(
            validate_ident("café"),
            Err(SqlBuildError::InvalidIdentifier {
                value: "café".to_string()
            })
        );
    }

    #[test]
    fn test_validate_ident_caps_length_at_postgres_limit() {
        // PostgreSQL truncates an identifier to 63 bytes, so a 64-byte name
        // and its 63-byte prefix would name the same table there.
        let longest = "a".repeat(MAX_IDENT_LEN);
        assert_eq!(validate_ident(&longest), Ok(longest.as_str()));
        let too_long = "a".repeat(MAX_IDENT_LEN + 1);
        assert_eq!(
            validate_ident(&too_long),
            Err(SqlBuildError::InvalidIdentifier {
                value: too_long.clone()
            })
        );
    }

    #[test]
    fn test_dyncol_iden() {
        let col = DynCol("my_col".into());
        let mut out = String::new();
        col.unquoted(&mut out);
        assert_eq!(out, "my_col");
    }
}
