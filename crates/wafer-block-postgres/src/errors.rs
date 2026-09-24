//! One classification of `sqlx` failures into [`DatabaseError`]s.

use wafer_core::interfaces::database::service::DatabaseError;

/// A failed `sqlx` call as a [`DatabaseError`].
///
/// - A unique violation (SQLSTATE `23505`, primary keys included) on a row of
///   a user table is [`DatabaseError::AlreadyExists`]. A `23505` on a
///   `pg_catalog` table is not a duplicate row: two sessions creating the same
///   table at once collide on a catalog index (`pg_type_typname_nsp_index`)
///   even under `IF NOT EXISTS`. That is a DDL race, and reporting it as a
///   taken key would misdirect the caller.
/// - A fault that says nothing about the request and may clear on its own is
///   [`DatabaseError::Unavailable`]: the server could not be reached or the
///   connection broke (an I/O error, SQLSTATE class `08` except `08P01`,
///   protocol_violation), no pooled
///   connection came free in time, the server is shutting down or starting up
///   (`57P01`–`57P03`), has no connection slot left (`53300`), or rolled the
///   transaction back to break a deadlock or a serialization conflict
///   (`40P01`, `40001`) or could not take a lock (`55P03`).
/// - Anything else is `Internal`.
pub(crate) fn sqlx_error(e: &sqlx::Error) -> DatabaseError {
    match e {
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut => DatabaseError::Unavailable(e.to_string()),
        sqlx::Error::Database(db) => {
            let code = db.code();
            let code = code.as_deref().unwrap_or_default();
            if db.is_unique_violation()
                && db
                    .try_downcast_ref::<sqlx::postgres::PgDatabaseError>()
                    .is_some_and(|pg| pg.schema().is_some_and(|schema| schema != "pg_catalog"))
            {
                DatabaseError::AlreadyExists(e.to_string())
            } else if is_transient_sqlstate(code) {
                DatabaseError::Unavailable(e.to_string())
            } else {
                DatabaseError::Internal(e.to_string())
            }
        }
        _ => DatabaseError::Internal(e.to_string()),
    }
}

fn is_transient_sqlstate(code: &str) -> bool {
    // `08P01` (protocol_violation) is in the connection class but is a bug on
    // one side of the wire, not a fault that clears.
    (code.starts_with("08") && code != "08P01")
        || matches!(
            code,
            "40001" | "40P01" | "53300" | "55P03" | "57P01" | "57P02" | "57P03"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_sqlstates_are_the_connection_and_contention_ones() {
        for code in [
            "08000", "08003", "08006", "08001", "08004", "40001", "40P01", "53300", "55P03",
            "57P01", "57P02", "57P03",
        ] {
            assert!(is_transient_sqlstate(code), "{code} is transient");
        }
        for code in [
            "08P01", "23505", "42P01", "42703", "22P02", "53100", "57014", "",
        ] {
            assert!(!is_transient_sqlstate(code), "{code} is not transient");
        }
    }

    #[test]
    fn io_and_pool_timeouts_are_unavailable_and_a_closed_pool_is_not() {
        let io = sqlx::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        assert!(matches!(sqlx_error(&io), DatabaseError::Unavailable(_)));
        assert!(matches!(
            sqlx_error(&sqlx::Error::PoolTimedOut),
            DatabaseError::Unavailable(_)
        ));
        assert!(matches!(
            sqlx_error(&sqlx::Error::PoolClosed),
            DatabaseError::Internal(_)
        ));
    }
}
