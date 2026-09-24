use sea_query::Iden;

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

/// Validate that `name` is a plain identifier — non-empty, ASCII
/// alphanumerics and underscore only — and return it unchanged.
///
/// This is the fail-closed guard for identifiers that have to be spliced
/// into raw SQL text rather than quoted or parameter-bound (index names,
/// `PRAGMA` arguments, vector table names, raw expression columns), and for
/// the table and column names a caller hands the shared SQL executor.
/// Anything outside the allowed set is rejected with
/// [`SqlBuildError::InvalidIdentifier`] instead of being silently
/// rewritten: character-stripping can turn one valid identifier into a
/// *different* valid identifier (`"users; DROP TABLE"` →
/// `"usersDROPTABLE"`), silently targeting the wrong object where the
/// request should have failed.
pub fn validate_ident(name: &str) -> Result<&str, SqlBuildError> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(SqlBuildError::InvalidIdentifier {
            value: name.to_string(),
        });
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_ident_accepts_plain_identifiers() {
        assert_eq!(validate_ident("users"), Ok("users"));
        assert_eq!(validate_ident("created_at"), Ok("created_at"));
        assert_eq!(validate_ident("Table123"), Ok("Table123"));
        assert_eq!(validate_ident("_private"), Ok("_private"));
    }

    #[test]
    fn test_validate_ident_rejects_injection_attempts() {
        for bad in ["users; DROP TABLE", "col\"name", "a-b", "a.b", "a b"] {
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
    fn test_dyncol_iden() {
        let col = DynCol("my_col".into());
        let mut out = String::new();
        col.unquoted(&mut out);
        assert_eq!(out, "my_col");
    }
}
