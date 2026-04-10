use sea_query::Iden;

/// Runtime identifier that implements sea_query::Iden.
/// Used for table and column names known only at runtime.
#[derive(Debug, Clone)]
pub struct DynCol(pub String);

impl Iden for DynCol {
    fn unquoted(&self, s: &mut dyn std::fmt::Write) {
        write!(s, "{}", self.0).unwrap();
    }
}

/// Sanitize an identifier to prevent SQL injection.
/// Only allows alphanumeric characters and underscores.
pub fn sanitize_ident(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_ident_normal() {
        assert_eq!(sanitize_ident("users"), "users");
        assert_eq!(sanitize_ident("created_at"), "created_at");
    }

    #[test]
    fn test_sanitize_ident_strips_dangerous() {
        assert_eq!(sanitize_ident("users; DROP TABLE"), "usersDROPTABLE");
        assert_eq!(sanitize_ident("col\"name"), "colname");
    }

    #[test]
    fn test_dyncol_iden() {
        let col = DynCol("my_col".into());
        let mut out = String::new();
        col.unquoted(&mut out);
        assert_eq!(out, "my_col");
    }
}
