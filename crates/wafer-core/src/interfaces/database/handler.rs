//! Shared message handler logic for database blocks.
//!
//! Any block implementing the `database@v1` interface can delegate to these
//! functions to avoid duplicating the message protocol handling.

use wafer_block::{
    common::{ErrorCode, ServiceOp},
    meta::META_WRAP_RESOURCE,
    streams::output::OutputStream,
    wire::database as wire,
    *,
};
use wafer_run::schema::Table;

use super::service::{
    self, DatabaseError, DatabaseService, Filter, FilterOp, ListOptions, SortField,
};
use crate::interfaces::handler_util::{decode_or_err, to_output};

/// SEC-003: enforce that the caller-supplied `wrap.resource` meta matches the
/// collection in the decoded payload. Empty meta = legacy path (runtime
/// already skipped WRAP); accept. The `__raw_sql__` / `__ddl__` pseudo-
/// resources are not checked here — they have their own admin/owner rules in
/// `wrap::check_access` that already gate query_raw/exec_raw/ddl.
fn check_collection(msg: &Message, expected: &str) -> Result<(), WaferError> {
    let supplied = msg.get_meta(META_WRAP_RESOURCE);
    if supplied.is_empty() || supplied == expected {
        Ok(())
    } else {
        Err(WaferError::new(
            ErrorCode::PERMISSION_DENIED,
            format!(
                "WRAP: wrap.resource meta '{supplied}' does not match payload collection '{expected}'"
            ),
        ))
    }
}

// --- Helpers ---

/// Parse a wire-format filter operator string into the typed [`FilterOp`].
///
/// Returns `Err` for unknown operators so the handler can surface
/// `INVALID_ARGUMENT` to the caller rather than silently coercing unknown
/// operators to `Equal` (which would change semantics of every malformed
/// query into a `WHERE field = value` match — see SEC-021).
fn parse_filter_op(op: &str) -> Result<FilterOp, WaferError> {
    match op {
        "eq" | "=" | "equal" => Ok(FilterOp::Equal),
        "neq" | "!=" | "not_equal" => Ok(FilterOp::NotEqual),
        "gt" | ">" | "greater_than" => Ok(FilterOp::GreaterThan),
        "gte" | ">=" | "greater_equal" => Ok(FilterOp::GreaterEqual),
        "lt" | "<" | "less_than" => Ok(FilterOp::LessThan),
        "lte" | "<=" | "less_equal" => Ok(FilterOp::LessEqual),
        "like" => Ok(FilterOp::Like),
        "in" => Ok(FilterOp::In),
        "is_null" => Ok(FilterOp::IsNull),
        "is_not_null" => Ok(FilterOp::IsNotNull),
        other => Err(WaferError::new(
            ErrorCode::INVALID_ARGUMENT,
            format!("unknown filter operator: {other:?}"),
        )),
    }
}

fn convert_filters(defs: Vec<wire::FilterDef>) -> Result<Vec<Filter>, WaferError> {
    defs.into_iter()
        .map(|f| {
            Ok(Filter {
                field: f.field,
                operator: parse_filter_op(&f.operator)?,
                value: f.value,
            })
        })
        .collect()
}

fn convert_sort(defs: Vec<wire::SortFieldDef>) -> Vec<SortField> {
    defs.into_iter()
        .map(|s| SortField {
            field: s.field,
            desc: s.desc,
        })
        .collect()
}

fn service_record_to_wire(r: service::Record) -> wire::Record {
    wire::Record {
        id: r.id,
        data: r.data,
    }
}

fn service_record_list_to_wire(l: service::RecordList) -> wire::RecordList {
    wire::RecordList {
        records: l.records.into_iter().map(service_record_to_wire).collect(),
        total_count: l.total_count,
        page: l.page,
        page_size: l.page_size,
    }
}

/// Substrings of structured backend errors that are safe to surface to
/// callers. These are operator-authored DDL outcomes (column names,
/// table names) — never user-supplied content — so they don't leak
/// secrets, and consumers like `solobase-core::migration_helper` need to
/// see them to decide whether a failure is benign (e.g. re-running an
/// `ALTER TABLE … ADD COLUMN` after the column already exists).
///
/// Every other internal error message stays scrubbed.
const PRESERVED_DB_ERROR_SUBSTRINGS: &[&str] = &[
    // SQLite / D1
    "duplicate column name",
    // PostgreSQL
    "already exists",
];

fn is_preserved_db_error(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    PRESERVED_DB_ERROR_SUBSTRINGS
        .iter()
        .any(|needle| lower.contains(needle))
}

fn db_error_to_wafer(e: DatabaseError) -> WaferError {
    match e {
        DatabaseError::NotFound => WaferError::new(ErrorCode::NOT_FOUND, "record not found"),
        DatabaseError::Internal(msg) => {
            if is_preserved_db_error(&msg) {
                tracing::warn!(error = %msg, "database structured error (preserved)");
                WaferError::new(ErrorCode::INTERNAL, msg)
            } else {
                tracing::error!(error = %msg, "database internal error");
                WaferError::new(ErrorCode::INTERNAL, "internal database error")
            }
        }
        DatabaseError::Other(err) => {
            let msg = err.to_string();
            if is_preserved_db_error(&msg) {
                tracing::warn!(error = %msg, "database structured error (preserved)");
                WaferError::new(ErrorCode::INTERNAL, msg)
            } else {
                tracing::error!(error = %msg, "database error");
                WaferError::new(ErrorCode::INTERNAL, "internal database error")
            }
        }
    }
}

/// Handle a database message using the given service.
pub async fn handle_message(
    service: &dyn DatabaseService,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::DATABASE_GET => {
            let req = decode_or_err!(body, wire::GetRequest, "database.get");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            match service.get(&req.collection, &req.id).await {
                Ok(record) => to_output(service_record_to_wire(record)),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_LIST => {
            let req = decode_or_err!(body, wire::ListRequest, "database.list");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            let filters = match convert_filters(req.filters) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            let opts = ListOptions {
                filters,
                sort: convert_sort(req.sort),
                limit: req.limit,
                offset: req.offset,
                skip_count: req.skip_count,
            };
            match service.list(&req.collection, &opts).await {
                Ok(list) => to_output(service_record_list_to_wire(list)),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_CREATE => {
            let req = decode_or_err!(body, wire::CreateRequest, "database.create");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            match service.create(&req.collection, req.data).await {
                Ok(record) => to_output(service_record_to_wire(record)),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_UPDATE => {
            let req = decode_or_err!(body, wire::UpdateRequest, "database.update");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            match service.update(&req.collection, &req.id, req.data).await {
                Ok(record) => to_output(service_record_to_wire(record)),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DELETE => {
            let req = decode_or_err!(body, wire::DeleteRequest, "database.delete");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            match service.delete(&req.collection, &req.id).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_COUNT => {
            let req = decode_or_err!(body, wire::CountRequest, "database.count");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            let filters = match convert_filters(req.filters) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            match service.count(&req.collection, &filters).await {
                Ok(count) => to_output(&wire::CountResponse { count }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_QUERY_RAW => {
            let req = decode_or_err!(body, wire::QueryRawRequest, "database.query_raw");
            match service.query_raw(&req.query, &req.args).await {
                Ok(records) => {
                    let wire_records: Vec<wire::Record> =
                        records.into_iter().map(service_record_to_wire).collect();
                    to_output(&wire_records)
                }
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_SUM => {
            let req = decode_or_err!(body, wire::SumRequest, "database.sum");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            let filters = match convert_filters(req.filters) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            match service.sum(&req.collection, &req.field, &filters).await {
                Ok(sum) => to_output(&wire::SumResponse { sum }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_EXEC_RAW => {
            let req = decode_or_err!(body, wire::ExecRawRequest, "database.exec_raw");
            match service.exec_raw(&req.query, &req.args).await {
                Ok(rows) => to_output(&wire::ExecRawResponse {
                    rows_affected: rows,
                }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DELETE_WHERE => {
            let req = decode_or_err!(body, wire::DeleteWhereRequest, "database.delete_where");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            let filters = match convert_filters(req.filters) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            match service.delete_where(&req.collection, &filters).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DELETE_WHERE_COUNT => {
            let req = decode_or_err!(
                body,
                wire::DeleteWhereCountRequest,
                "database.delete_where_count"
            );
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            let filters = match convert_filters(req.filters) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            match service.delete_where_count(&req.collection, &filters).await {
                Ok(count) => to_output(&wire::DeleteWhereCountResponse { count }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_TAKE_WHERE => {
            let req = decode_or_err!(body, wire::TakeWhereRequest, "database.take_where");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            let filters = match convert_filters(req.filters) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            match service.take_where(&req.collection, &filters).await {
                Ok(records) => to_output(&wire::TakeWhereResponse {
                    records: records.into_iter().map(service_record_to_wire).collect(),
                }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_UPDATE_WHERE => {
            let req = decode_or_err!(body, wire::UpdateWhereRequest, "database.update_where");
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            let filters = match convert_filters(req.filters) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            match service
                .update_where(&req.collection, &filters, req.data)
                .await
            {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_INCREMENT_FIELD_WHERE => {
            let req = decode_or_err!(
                body,
                wire::IncrementFieldWhereRequest,
                "database.increment_field_where"
            );
            if let Err(e) = check_collection(msg, &req.collection) {
                return OutputStream::error(e);
            }
            let filters = match convert_filters(req.filters) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            match service
                .increment_field_where(&req.collection, &req.col, req.delta, &filters)
                .await
            {
                Ok(rows) => to_output(&wire::ExecRawResponse {
                    rows_affected: rows,
                }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::UNIMPLEMENTED,
            format!("unknown database operation: {other}"),
        )),
    }
}

/// Handle database lifecycle events (schema migration on Init).
pub async fn handle_lifecycle(
    service: &dyn DatabaseService,
    tables: &[Table],
    event: &LifecycleEvent,
) -> std::result::Result<(), WaferError> {
    if event.event_type == LifecycleType::Init {
        if tables.is_empty() {
            tracing::debug!("no schema tables configured — skipping migration");
        } else {
            service.ensure_schema_tables(tables).await.map_err(|e| {
                WaferError::new(ErrorCode::INTERNAL, format!("schema migration failed: {e}"))
            })?;
            tracing::info!(tables = tables.len(), "database schema migrations applied");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_duplicate_column_error_messages() {
        // SQLite / D1 wording
        let w = db_error_to_wafer(DatabaseError::Internal(
            "duplicate column name: block".to_string(),
        ));
        assert_eq!(w.code, ErrorCode::INTERNAL);
        assert!(
            w.message.contains("duplicate column name"),
            "expected preserved message, got: {}",
            w.message
        );

        // PostgreSQL wording
        let w = db_error_to_wafer(DatabaseError::Internal(
            r#"column "block" of relation "variables" already exists"#.to_string(),
        ));
        assert!(
            w.message.contains("already exists"),
            "expected preserved message, got: {}",
            w.message
        );
    }

    #[test]
    fn scrubs_generic_internal_errors() {
        // Random backend internal failures still get scrubbed so we don't
        // leak driver internals or connection strings.
        let w = db_error_to_wafer(DatabaseError::Internal(
            "connection refused: tcp://10.0.0.5:5432".to_string(),
        ));
        assert_eq!(w.message, "internal database error");
    }

    #[test]
    fn not_found_stays_descriptive() {
        let w = db_error_to_wafer(DatabaseError::NotFound);
        assert_eq!(w.code, ErrorCode::NOT_FOUND);
        assert_eq!(w.message, "record not found");
    }

    #[test]
    fn parse_filter_op_known_ops() {
        assert!(matches!(parse_filter_op("eq"), Ok(FilterOp::Equal)));
        assert!(matches!(parse_filter_op("="), Ok(FilterOp::Equal)));
        assert!(matches!(parse_filter_op("neq"), Ok(FilterOp::NotEqual)));
        assert!(matches!(parse_filter_op("like"), Ok(FilterOp::Like)));
        assert!(matches!(parse_filter_op("is_null"), Ok(FilterOp::IsNull)));
    }

    #[test]
    fn parse_filter_op_rejects_unknown() {
        let err = parse_filter_op("bogus").expect_err("unknown op must be rejected");
        assert_eq!(err.code, ErrorCode::INVALID_ARGUMENT);
        assert!(
            err.message.contains("unknown filter operator"),
            "message: {}",
            err.message
        );

        // Empty string also rejected (was previously coerced to Equal).
        let err = parse_filter_op("").expect_err("empty op must be rejected");
        assert_eq!(err.code, ErrorCode::INVALID_ARGUMENT);
    }

    #[test]
    fn convert_filters_rejects_bad_op() {
        let defs = vec![
            wire::FilterDef {
                field: "id".into(),
                operator: "eq".into(),
                value: serde_json::json!(1),
            },
            wire::FilterDef {
                field: "name".into(),
                operator: "nope".into(),
                value: serde_json::json!("x"),
            },
        ];
        let err = convert_filters(defs).expect_err("bad op should fail conversion");
        assert_eq!(err.code, ErrorCode::INVALID_ARGUMENT);
    }
}
