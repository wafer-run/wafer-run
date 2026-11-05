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
/// - The server unable to answer is [`DatabaseError::Unavailable`]: it could
///   not be reached or the connection broke (an I/O error, SQLSTATE class
///   `08` except `08P01`, protocol_violation), or it is shutting down
///   (`57P01`, `57P02`) — or a pool timeout. sqlx answers `PoolTimedOut`
///   both when every connection of the pool is checked out by the
///   application's own work and when it could not open a connection at all,
///   and exposes no count of established connections that would tell them
///   apart (the pool's size includes connections still being opened). So a
///   pool timeout is always `Unavailable`; a pool exhausted by the
///   application's own work reads as the database down. The server refusing
///   a new connection (`53300` too_many_connections, `57P03`
///   cannot_connect_now) never reaches here as itself: sqlx retries both
///   while connecting, and they surface as that pool timeout.
/// - The server up but refusing this statement because of other work is
///   [`DatabaseError::Contended`]: it rolled the transaction back to break a
///   deadlock or a serialization conflict (`40P01`, `40001`), or could not
///   take a lock (`55P03`).
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
            } else if is_unavailable_sqlstate(code) {
                DatabaseError::Unavailable(e.to_string())
            } else if is_contention_sqlstate(code) {
                DatabaseError::Contended(e.to_string())
            } else {
                DatabaseError::Internal(e.to_string())
            }
        }
        _ => DatabaseError::Internal(e.to_string()),
    }
}

/// Whether PostgreSQL refused to run a connection's cached prepared statement
/// because the statement's result columns changed since it was prepared:
/// `cached plan must not change result type`, SQLSTATE `0A000`
/// (feature_not_supported), raised by the server's `RevalidateCachedQuery`.
///
/// A prepared statement's result columns are fixed when it is prepared. Once
/// a table it reads gains or loses a column, by DDL on any session of any
/// process, every session still holding it gets this error the next time it
/// runs the statement. The check fires while the statement is bound, before
/// it runs, so the statement had no effect: re-preparing it and running it
/// again is safe. `0A000` alone also covers features the server does not
/// support at all, so the reporting routine (sent with every error, and not
/// translated as the message text is) narrows it to this one case.
pub(crate) fn is_stale_plan(e: &sqlx::Error) -> bool {
    let sqlx::Error::Database(db) = e else {
        return false;
    };
    db.try_downcast_ref::<sqlx::postgres::PgDatabaseError>()
        .is_some_and(|pg| pg.code() == "0A000" && pg.routine() == Some("RevalidateCachedQuery"))
}

/// The server could not answer: a connection fault, no slot left, shutting
/// down or starting up.
fn is_unavailable_sqlstate(code: &str) -> bool {
    // `08P01` (protocol_violation) is in the connection class but is a bug on
    // one side of the wire, not a fault that clears.
    (code.starts_with("08") && code != "08P01") || matches!(code, "57P01" | "57P02")
}

/// The server up, refusing this statement because of other work: a
/// serialization conflict or deadlock broken, a lock not taken.
fn is_contention_sqlstate(code: &str) -> bool {
    matches!(code, "40001" | "40P01" | "55P03")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_down_is_unavailable_and_contention_is_contended() {
        for code in [
            "08000", "08003", "08006", "08001", "08004", "57P01", "57P02",
        ] {
            assert!(is_unavailable_sqlstate(code), "{code} is the server down");
            assert!(!is_contention_sqlstate(code), "{code} is not contention");
        }
        for code in ["40001", "40P01", "55P03"] {
            assert!(is_contention_sqlstate(code), "{code} is contention");
            assert!(
                !is_unavailable_sqlstate(code),
                "{code} is not the server down"
            );
        }
        // `53300` and `57P03` are refused connections, which sqlx retries
        // while connecting and reports as a pool timeout: never mapped here.
        for code in [
            "08P01", "23505", "42P01", "42703", "22P02", "53100", "57014", "53300", "57P03", "",
        ] {
            assert!(
                !is_unavailable_sqlstate(code) && !is_contention_sqlstate(code),
                "{code} is not transient"
            );
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
