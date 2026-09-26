//! Shared message handler logic for database blocks.
//!
//! Any block implementing the `database@v1` interface can delegate to these
//! functions to avoid duplicating the message protocol handling.
//!
//! Every collection and column a request names must be a plain identifier —
//! at most 63 bytes of ASCII lowercase letters, digits and `_`
//! ([`check_name`]) — or the request is `InvalidArgument`. A collection name
//! is checked before the caller is authorized on it, and the executor puts the authorized string into SQL
//! byte for byte, so the table a request touches is the table it was
//! authorized on. Nothing rewrites a name: stripping `-` from `acme__a-b__t`
//! would authorize the caller as `acme/a-b` and run the statement on
//! `acme/ab`'s table `acme__ab__t`.

use std::collections::HashMap;

use wafer_block::{
    common::{ErrorCode, ServiceOp},
    db::{ColumnCompareOp, ColumnFilter, Filter, FilterOp, FilterTree, ListOptions, SortField},
    streams::output::OutputStream,
    types::ResourceType,
    wire::database as wire,
    wrap::{database_op_access, DatabaseOpAccess, DDL_RESOURCE, RAW_SQL_RESOURCE, SCHEMA_RESOURCE},
    *,
};
use wafer_schema::Table;
use wafer_sql_utils::aggregate::CastType;

use super::{
    exec::windowed_counter_row,
    schema_wire,
    service::{self, DatabaseError, DatabaseService},
};
use crate::interfaces::handler_util::{decode_and_authorize_all, to_output};

// --- Helpers ---

/// Config key for the `wafer-run/database` block's STRICT_SCHEMA mode. When
/// enabled, SQL backends trust their migrated schema and skip per-operation
/// schema introspection (see
/// [`DbExec::strict_schema`](super::exec::DbExec::strict_schema)). Declared as
/// a `ConfigVar` on that block and read from its Init config
/// ([`strict_schema_from`]); a backend block that is its own `database@v1`
/// block (`wafer-run/postgres`) declares a key under its own prefix.
pub const STRICT_SCHEMA_CONFIG_KEY: &str = "WAFER_RUN__DATABASE__STRICT_SCHEMA";

/// Whether the `lifecycle(Init)` config of `event` turns STRICT_SCHEMA on
/// under `key` — the block's own declared key, which the runtime resolved
/// through the embedder's `ConfigSource` into the Init payload. Absent is
/// off.
pub fn strict_schema_from(event: &LifecycleEvent, key: &str) -> bool {
    config_flag_enabled(wafer_block::config::BlockConfig::from_event(event).str(key))
}

/// Interpret a config string as a boolean flag: `"true"`/`"1"`
/// (case-insensitive, trimmed) enable it; anything else — including an unset
/// key — is `false`. Matches the convention used by other toggle config vars.
fn config_flag_enabled(value: &str) -> bool {
    let v = value.trim();
    v.eq_ignore_ascii_case("true") || v == "1"
}

/// Maximum nesting depth of a `FilterNode` tree accepted from the wire.
pub(crate) const MAX_FILTER_DEPTH: usize = 16;
/// Maximum total node count of a `FilterNode` tree accepted from the wire.
pub(crate) const MAX_FILTER_NODES: usize = 256;

fn invalid(msg: impl Into<String>) -> WaferError {
    WaferError::new(ErrorCode::InvalidArgument, msg)
}

/// Convert a wire `FilterNode` forest into builder-input `FilterTree`,
/// rejecting trees that exceed the depth or node-count bounds, reject unknown
/// filter operators, reject malformed column-to-column leaves (see
/// [`convert_leaf`]), and reject nested empty `all`/`any` groups (which would
/// otherwise collapse to a degenerate always-true/always-false condition).
/// Total and panic-free on any input.
///
/// A top-level empty forest (`[]`) is valid and means "no filter"; only
/// *nested* empty groups are rejected.
///
/// Public (like [`flatten_leaves`], [`to_upsert_spec`], [`to_aggregate_spec`])
/// as the render trust boundary's wire→builder-input conversion surface: it
/// takes only wire types and is exercised directly by the wire-round-trip
/// render-parity integration tests (`tests/database_render_parity.rs`), which
/// prove SP-B2's future migration is pure re-plumbing.
pub fn convert_filter_tree(nodes: Vec<wire::FilterNode>) -> Result<Vec<FilterTree>, WaferError> {
    let mut count = 0usize;
    nodes
        .into_iter()
        .map(|n| convert_node(n, 1, &mut count))
        .collect()
}

fn convert_node(
    node: wire::FilterNode,
    depth: usize,
    count: &mut usize,
) -> Result<FilterTree, WaferError> {
    if depth > MAX_FILTER_DEPTH {
        return Err(invalid("filter tree too deep"));
    }
    *count += 1;
    if *count > MAX_FILTER_NODES {
        return Err(invalid("filter tree has too many nodes"));
    }
    match node {
        wire::FilterNode::Leaf(f) => convert_leaf(f),
        wire::FilterNode::All { all } => {
            if all.is_empty() {
                return Err(invalid("filter group must have at least one child"));
            }
            Ok(FilterTree::All(
                all.into_iter()
                    .map(|c| convert_node(c, depth + 1, count))
                    .collect::<Result<_, _>>()?,
            ))
        }
        wire::FilterNode::Any { any } => {
            if any.is_empty() {
                return Err(invalid("filter group must have at least one child"));
            }
            Ok(FilterTree::Any(
                any.into_iter()
                    .map(|c| convert_node(c, depth + 1, count))
                    .collect::<Result<_, _>>()?,
            ))
        }
    }
}

/// Validate one [`wire::FilterDef`] and convert it: a value leaf to
/// [`FilterTree::Leaf`], or — when `column` is set — a column-to-column leaf to
/// [`FilterTree::ColumnCompare`].
///
/// A leaf whose `field` fails [`check_name`] is rejected as
/// `InvalidArgument`. The column form is rejected too when it also carries a
/// non-null `value` (the two are mutually exclusive), when its operator has no
/// column form (`like`, `in`, `is_null`, `is_not_null`), or when its `column`
/// fails [`check_name`].
fn convert_leaf(f: wire::FilterDef) -> Result<FilterTree, WaferError> {
    let operator = FilterOp::parse_wire(&f.operator).map_err(|e| invalid(e.to_string()))?;
    check_name(&f.field)?;
    let Some(column) = f.column else {
        return Ok(FilterTree::Leaf(Filter {
            field: f.field,
            operator,
            value: f.value,
        }));
    };
    if !f.value.is_null() {
        return Err(invalid(
            "a filter compares its field to either `value` or `column`, not both",
        ));
    }
    let Some(operator) = ColumnCompareOp::from_filter_op(&operator) else {
        return Err(invalid(format!(
            "filter operator {:?} cannot compare two columns",
            f.operator
        )));
    };
    check_name(&column)?;
    Ok(FilterTree::ColumnCompare(ColumnFilter {
        field: f.field,
        operator,
        column,
    }))
}

/// Admit `name` as a collection, column or alias name only when it is a plain
/// identifier ([`wafer_block::db::is_plain_ident`]: non-empty, at most 63
/// bytes, ASCII lowercase letters, digits and `_`). Anything else is
/// `InvalidArgument`, never rewritten (see the module docs). The executor
/// applies the same rule (`wafer_sql_utils::ident::validate_ident`), so a
/// caller that reaches it without this handler is held to it too.
pub(super) fn check_name(name: &str) -> Result<(), WaferError> {
    if wafer_block::db::is_plain_ident(name) {
        return Ok(());
    }
    Err(invalid(format!(
        "{name:?} is not a collection or column name (1 to 63 of: lowercase letters, digits \
         and `_`)"
    )))
}

/// [`check_name`] every key of `data`: the columns a write sets.
fn check_data_names(data: &HashMap<String, serde_json::Value>) -> Result<(), WaferError> {
    data.keys().try_for_each(|key| check_name(key))
}

/// Flatten a tree to a leaf-only `Vec<Filter>`, rejecting any group node and
/// any column-to-column leaf. Used by ops whose builders take a flat
/// `&[Filter]`, which can represent neither; either one here is a
/// client/runtime mismatch, so fail closed rather than silently drop it.
///
/// Public as part of the wire→builder-input conversion surface (see
/// [`convert_filter_tree`]); asserted against direct builder calls in
/// `tests/database_render_parity.rs`.
pub fn flatten_leaves(tree: &[FilterTree]) -> Result<Vec<Filter>, WaferError> {
    let mut out = Vec::with_capacity(tree.len());
    for node in tree {
        match node {
            FilterTree::Leaf(f) => out.push(f.clone()),
            FilterTree::ColumnCompare(_) => {
                return Err(invalid(
                    "operation does not support column-to-column filters",
                ));
            }
            FilterTree::All(_) | FilterTree::Any(_) => {
                return Err(invalid("operation does not support filter groups"));
            }
        }
    }
    Ok(out)
}

/// Convert a wire [`wire::UpsertRequest`] into a `(collection, UpsertSpec)`
/// pair for [`DatabaseService::upsert`], validating **every** identifier that
/// could reach raw SQL text.
///
/// `data` values are parameter-bound and `SetColumns`/`conflict_columns` reach
/// sea-query as quoted `DynCol`s, but we validate *all* column identifiers
/// uniformly (via [`check_name`], `InvalidArgument` on failure) so a hostile
/// name can never be interpolated — the
/// `WindowedCounter` builder in particular splices `count_field`/`window_field`
/// and the timestamp columns into `CASE`/`SET` expression text, where binding
/// is impossible. Returns the collection alongside the spec so the caller can
/// authorize/dispatch without a move-after-use of `req.collection`.
///
/// `WindowedCounter` also requires exactly one conflict column and `data`
/// holding a string `id`, a string for that column and nothing else
/// ([`windowed_counter_row`], the check `DbExec::upsert` repeats) — anything
/// the statement would not write is `InvalidArgument` here, before a backend
/// sees it.
///
/// Public as part of the wire→builder-input conversion surface (see
/// [`convert_filter_tree`]): it takes only the wire request and is compared
/// against a direct `upsert::build_upsert` / `build_windowed_counter_upsert`
/// call in `tests/database_render_parity.rs`.
pub fn to_upsert_spec(
    req: wire::UpsertRequest,
) -> Result<(String, service::UpsertSpec), WaferError> {
    for (col, _) in &req.data {
        check_name(col)?;
    }
    for col in &req.conflict_columns {
        check_name(col)?;
    }

    let on_conflict = match req.on_conflict {
        wire::OnConflict::SetColumns(cols) => {
            for col in &cols {
                check_name(col)?;
            }
            service::UpsertConflict::SetColumns(cols)
        }
        wire::OnConflict::WindowedCounter {
            count_field,
            window_field,
            now,
            window_cutoff,
            created_fields,
            updated_fields,
        } => {
            check_name(&count_field)?;
            check_name(&window_field)?;
            for col in created_fields.iter().chain(&updated_fields) {
                check_name(col)?;
            }
            windowed_counter_row(&req.data, &req.conflict_columns).map_err(db_error_to_wafer)?;
            service::UpsertConflict::WindowedCounter {
                count_field,
                window_field,
                now,
                window_cutoff,
                created_fields,
                updated_fields,
            }
        }
    };

    Ok((
        req.collection,
        service::UpsertSpec {
            data: req.data,
            conflict_columns: req.conflict_columns,
            on_conflict,
        },
    ))
}

/// Convert one wire [`wire::BatchWrite`] into the service's
/// [`WriteOp`](service::WriteOp), validating it exactly as its single-op arm
/// does: data keys pass [`check_name`], `UpdateWhere` and `DeleteWhere`
/// filters are bounded and flattened to AND-of-leaves (a group or a
/// column-to-column leaf is `InvalidArgument`, as for `database.update_where`
/// and `database.delete_where`), and `Upsert` goes
/// through [`to_upsert_spec`]. The collection was checked before the batch
/// was authorized.
fn to_write_op(write: wire::BatchWrite) -> Result<service::WriteOp, WaferError> {
    Ok(match write {
        wire::BatchWrite::Create { collection, data } => {
            check_data_names(&data)?;
            service::WriteOp::Create { collection, data }
        }
        wire::BatchWrite::Update {
            collection,
            id,
            data,
        } => {
            check_data_names(&data)?;
            service::WriteOp::Update {
                collection,
                id,
                data,
            }
        }
        wire::BatchWrite::Delete { collection, id } => service::WriteOp::Delete { collection, id },
        wire::BatchWrite::UpdateWhere {
            collection,
            filters,
            data,
        } => {
            check_data_names(&data)?;
            service::WriteOp::UpdateWhere {
                collection,
                filters: flatten_leaves(&convert_filter_tree(filters)?)?,
                data,
            }
        }
        wire::BatchWrite::DeleteWhere {
            collection,
            filters,
        } => service::WriteOp::DeleteWhere {
            collection,
            filters: flatten_leaves(&convert_filter_tree(filters)?)?,
        },
        wire::BatchWrite::Upsert(req) => {
            let (collection, spec) = to_upsert_spec(req)?;
            service::WriteOp::Upsert { collection, spec }
        }
    })
}

/// Convert wire [`wire::CapGuard`]s into the service's
/// [`CapGuard`](service::CapGuard)s, refusing more than
/// [`wire::MAX_WRITE_GUARDS`], validating each guard's filters as
/// `database.update_where`'s (bounded, AND-of-leaves), and the `SumAtMost`
/// field as a plain identifier.
fn to_cap_guards(guards: Vec<wire::CapGuard>) -> Result<Vec<service::CapGuard>, WaferError> {
    if guards.len() > wire::MAX_WRITE_GUARDS {
        return Err(invalid(format!(
            "{} guards; at most {} per call",
            guards.len(),
            wire::MAX_WRITE_GUARDS
        )));
    }
    guards
        .into_iter()
        .map(|guard| {
            Ok(match guard {
                wire::CapGuard::CountBelow { filters, cap } => service::CapGuard::CountBelow {
                    filters: flatten_leaves(&convert_filter_tree(filters)?)?,
                    cap,
                },
                wire::CapGuard::SumAtMost {
                    field,
                    filters,
                    add,
                    cap,
                } => {
                    check_name(&field)?;
                    service::CapGuard::SumAtMost {
                        field,
                        filters: flatten_leaves(&convert_filter_tree(filters)?)?,
                        add,
                        cap,
                    }
                }
            })
        })
        .collect()
}

fn write_outcome_to_wire(outcome: service::WriteOutcome) -> wire::BatchWriteResult {
    match outcome {
        service::WriteOutcome::Created(r) => {
            wire::BatchWriteResult::Created(service_record_to_wire(r))
        }
        service::WriteOutcome::Updated(r) => {
            wire::BatchWriteResult::Updated(r.map(service_record_to_wire))
        }
        service::WriteOutcome::Deleted { rows_affected } => {
            wire::BatchWriteResult::Deleted { rows_affected }
        }
        service::WriteOutcome::UpdatedWhere { rows_affected } => {
            wire::BatchWriteResult::UpdatedWhere { rows_affected }
        }
        service::WriteOutcome::DeletedWhere { rows_affected } => {
            wire::BatchWriteResult::DeletedWhere { rows_affected }
        }
        service::WriteOutcome::Upserted { rows_affected } => {
            wire::BatchWriteResult::Upserted { rows_affected }
        }
    }
}

/// Convert a wire [`wire::AggregateRequest`] into a `(collection,
/// AggregateSpec)` pair for [`DatabaseService::aggregate`], validating **every**
/// identifier that could reach raw SQL text.
///
/// Aliases, aggregated `Sum`/`Avg`/`Max`/`SumWhere` `field`s,
/// `DateBucket.field`s, plain `GroupByDef::Column`s, and `select_columns` are
/// all interpolated as identifiers (aliases/date-bucket fields reach *raw*
/// `date(...)` / `AS <alias>` expression text where binding is impossible), so
/// each is validated via [`check_name`] — a failure is `InvalidArgument`,
/// fail-closed. A `cast_as` type name is spliced
/// into `CAST(... AS <type>)` text, so it is parsed against the
/// [`CastType`] allowlist — every member for `Sum`/`SumWhere`, only
/// `DOUBLE PRECISION` for `Avg` — and anything else is `InvalidArgument`.
/// `CaseWhenSum.when` and `SumWhere.when` are run through
/// [`convert_filter_tree`] for depth/node bounds + operator validation (and
/// rejected if empty); their `!Send` `CASE` predicates are built server-side in
/// [`AggregateSpec::into_grouped_config`], so the spec carries the validated
/// [`FilterTree`] forests, not sea-query expressions. `filters` are flattened
/// to AND-of-leaves (a group or a column-to-column leaf → `InvalidArgument`,
/// consistent with `count`/`sum`).
///
/// Public as part of the wire→builder-input conversion surface (see
/// [`convert_filter_tree`]): it takes only the wire request and its output,
/// fed through `AggregateSpec::into_grouped_config`, is compared against a
/// direct `aggregate::build_grouped_query` call in
/// `tests/database_render_parity.rs`.
pub fn to_aggregate_spec(
    req: wire::AggregateRequest,
) -> Result<(String, service::AggregateSpec), WaferError> {
    for col in &req.select_columns {
        check_name(col)?;
    }

    let mut aggregates = Vec::with_capacity(req.aggregates.len());
    for agg in req.aggregates {
        let spec = match agg {
            wire::AggregateColumnDef::Count { alias } => {
                check_name(&alias)?;
                service::AggregateColumnSpec::Count { alias }
            }
            wire::AggregateColumnDef::Sum {
                field,
                alias,
                cast_as,
            } => {
                check_name(&field)?;
                check_name(&alias)?;
                service::AggregateColumnSpec::Sum {
                    field,
                    alias,
                    cast_as: parse_cast(cast_as.as_deref(), &CastType::ALL)?,
                }
            }
            wire::AggregateColumnDef::Avg {
                field,
                alias,
                cast_as,
            } => {
                check_name(&field)?;
                check_name(&alias)?;
                // An average is rarely integral, and `BIGINT` rounds it on
                // Postgres but truncates it on SQLite — the same request would
                // answer differently per backend — so `Avg` casts to
                // `DOUBLE PRECISION` only.
                service::AggregateColumnSpec::Avg {
                    field,
                    alias,
                    cast_as: parse_cast(cast_as.as_deref(), &[CastType::Double])?,
                }
            }
            wire::AggregateColumnDef::Max { field, alias } => {
                check_name(&field)?;
                check_name(&alias)?;
                service::AggregateColumnSpec::Max { field, alias }
            }
            wire::AggregateColumnDef::CaseWhenSum { when, alias } => {
                check_name(&alias)?;
                service::AggregateColumnSpec::CaseWhenSum {
                    when: convert_when(when, "case-when-sum")?,
                    alias,
                }
            }
            wire::AggregateColumnDef::SumWhere {
                field,
                when,
                alias,
                cast_as,
            } => {
                check_name(&field)?;
                check_name(&alias)?;
                service::AggregateColumnSpec::SumWhere {
                    field,
                    when: convert_when(when, "sum-where")?,
                    alias,
                    cast_as: parse_cast(cast_as.as_deref(), &CastType::ALL)?,
                }
            }
        };
        aggregates.push(spec);
    }

    let mut group_by = Vec::with_capacity(req.group_by.len());
    for g in req.group_by {
        let spec = match g {
            wire::GroupByDef::Column(c) => {
                check_name(&c)?;
                service::GroupBySpec::Column(c)
            }
            wire::GroupByDef::DateBucket { field } => {
                check_name(&field)?;
                service::GroupBySpec::DateBucket { field }
            }
        };
        group_by.push(spec);
    }

    // Aggregation filters are AND-of-leaves; a group or a column-to-column
    // leaf here is a client/runtime mismatch → InvalidArgument (same rule as
    // count/sum).
    let tree = convert_filter_tree(req.filters)?;
    let filters = flatten_leaves(&tree)?;

    let spec = service::AggregateSpec {
        select_columns: req.select_columns,
        aggregates,
        filters,
        group_by,
        sort: convert_sort(req.sort)?,
        limit: req.limit,
    };
    Ok((req.collection, spec))
}

/// Bound and validate a conditional aggregate's `when` forest, rejecting an
/// empty one. The `SimpleExpr` itself is built server-side (it is `!Send`).
fn convert_when(when: Vec<wire::FilterNode>, kind: &str) -> Result<Vec<FilterTree>, WaferError> {
    let tree = convert_filter_tree(when)?;
    if tree.is_empty() {
        return Err(invalid(format!(
            "{kind} aggregate requires at least one predicate in `when`"
        )));
    }
    Ok(tree)
}

/// Parse an aggregate's optional output cast against `allowed`, a subset of
/// the [`CastType`] allowlist. The type name is spliced into
/// `CAST(... AS <type>)` text, so anything off the list is `InvalidArgument`.
fn parse_cast(cast_as: Option<&str>, allowed: &[CastType]) -> Result<Option<CastType>, WaferError> {
    let Some(name) = cast_as else {
        return Ok(None);
    };
    CastType::parse(name)
        .filter(|t| allowed.contains(t))
        .map(Some)
        .ok_or_else(|| {
            let allowed: Vec<&str> = allowed.iter().map(|t| t.as_sql()).collect();
            invalid(format!(
                "aggregate cast_as {name:?} is not one of {allowed:?}"
            ))
        })
}

/// Convert wire sort keys, refusing a field that fails [`check_name`].
fn convert_sort(defs: Vec<wire::SortFieldDef>) -> Result<Vec<SortField>, WaferError> {
    defs.into_iter()
        .map(|s| {
            check_name(&s.field)?;
            Ok(SortField {
                field: s.field,
                desc: s.desc,
            })
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
/// secrets, and consumers like an application's `migration_helper` need to
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
    let code = e.code();
    let detail = e.detail_code();
    let err = match e {
        DatabaseError::NotFound => WaferError::new(code, "record not found"),
        // The driver's message names the constraint and its columns, which is
        // schema, not the caller's concern; log it, answer with the code.
        DatabaseError::AlreadyExists(msg) => {
            tracing::debug!(error = %msg, "database unique constraint violated");
            WaferError::new(code, "a record with this key already exists")
        }
        // The executor's message names only what the caller sent (a table, a
        // column, a limit or offset), so it goes back to the caller as is.
        DatabaseError::InvalidArgument(msg) => WaferError::new(code, msg),
        // Names only statement counts and the backend's limit.
        DatabaseError::StatementLimitExceeded(msg) | DatabaseError::ResourceExhausted(msg) => {
            WaferError::new(code, msg)
        }
        // Transient: the caller may retry (and the runtime retries a block
        // Init that failed this way instead of caching the failure). The
        // driver's message can name hosts and files, so it is logged, not
        // returned.
        DatabaseError::Unavailable(msg) => {
            tracing::warn!(error = %msg, "database temporarily unavailable");
            WaferError::new(code, "database temporarily unavailable")
        }
        DatabaseError::Internal(msg) => {
            if is_preserved_db_error(&msg) {
                tracing::warn!(error = %msg, "database structured error (preserved)");
                WaferError::new(code, msg)
            } else {
                tracing::error!(error = %msg, "database internal error");
                WaferError::new(code, "internal database error")
            }
        }
        DatabaseError::Other(err) => {
            let msg = err.to_string();
            if is_preserved_db_error(&msg) {
                tracing::warn!(error = %msg, "database structured error (preserved)");
                WaferError::new(code, msg)
            } else {
                tracing::error!(error = %msg, "database error");
                WaferError::new(code, "internal database error")
            }
        }
    };
    // The detail code tells a caller what the coarse code cannot: a
    // statement-budget refusal shares its code with other refusals (any
    // `InvalidArgument`; a rate limit's or the call-depth limit's
    // `ResourceExhausted`).
    match detail {
        Some(detail) => err.with_detail_code(detail),
        None => err,
    }
}

/// The WRAP checks `op` needs on `resource`, as
/// [`wafer_block::wrap::DATABASE_OP_ACCESS`] classifies it. A `resource` that
/// fails [`check_name`] is refused before any check is listed, so the name
/// authorized is the name the executor runs on. An op the table does not
/// classify for a single resource is refused rather than run unchecked.
fn op_checks(
    op: &str,
    resource: &str,
) -> Result<Vec<(String, ResourceType, ResourceAccess)>, WaferError> {
    check_name(resource)?;
    match database_op_access(op) {
        Some(DatabaseOpAccess::On(accesses)) => Ok(accesses
            .iter()
            .map(|access| (resource.to_string(), ResourceType::Db, *access))
            .collect()),
        Some(DatabaseOpAccess::PerWrite) | None => Err(WaferError::new(
            ErrorCode::Internal,
            format!("BUG: DATABASE_OP_ACCESS names no single-resource access for `{op}`"),
        )),
    }
}

/// Columns the server fills on every inserted row. A caller inserting
/// through an append-only grant may not supply them: an audit trail whose
/// grantees could pick a row's `id` or back-date its timestamps would record
/// whatever history they chose.
const SERVER_OWNED_COLUMNS: [&str; 3] = ["id", "created_at", "updated_at"];

/// Whether the caller — already authorized to append to `collection` —
/// holds only that: no `Write` on it. Such an insert follows the
/// append-only rules of [`check_append_only_rows`].
fn inserts_append_only(ctx: &dyn Context, collection: &str) -> bool {
    !ctx.resource_access_admitted(collection, ResourceType::Db, ResourceAccess::Write)
}

/// The rules an insert through an append-only grant must satisfy, checked
/// before the service runs so a refused insert changes nothing:
/// - it names only column names that pass [`check_name`];
/// - it names none of [`SERVER_OWNED_COLUMNS`], which the server stamps;
/// - every column it would write — the ones it names and the server-owned
///   ones — already exists. Outside STRICT_SCHEMA the service adds an unseen
///   column on insert, and the value's type fixes the column's type; that
///   reshapes the owner's table, which an append-only grant does not confer.
///   Columns are never dropped individually, so one present here is present
///   when the insert runs.
///
/// A guarded insert's guard columns need no rule here: a guard never adds a
/// column, the executor refuses one the table lacks.
///
/// So a collection lacking any of `id`, `created_at` or `updated_at` refuses
/// EVERY append-only insert — the server would stamp the missing column and,
/// outside STRICT_SCHEMA, add it. An owner that grants append declares all
/// three columns.
async fn check_append_only_rows<'a>(
    service: &dyn DatabaseService,
    collection: &str,
    rows: impl IntoIterator<Item = &'a HashMap<String, serde_json::Value>>,
) -> Result<(), WaferError> {
    let mut named: Vec<String> = Vec::new();
    for row in rows {
        for key in row.keys() {
            check_name(key)?;
            if SERVER_OWNED_COLUMNS.contains(&key.as_str()) {
                return Err(WaferError::new(
                    ErrorCode::PermissionDenied,
                    format!(
                        "an append-only insert into `{collection}` cannot set `{key}`; \
                         the server assigns it"
                    ),
                ));
            }
            if !named.contains(key) {
                named.push(key.clone());
            }
        }
    }
    let existing = service
        .schema_columns(collection)
        .await
        .map_err(db_error_to_wafer)?;
    let missing = SERVER_OWNED_COLUMNS
        .iter()
        .map(|c| (*c).to_string())
        .chain(named)
        .find(|c| !existing.contains(c));
    match missing {
        Some(column) => Err(WaferError::new(
            ErrorCode::PermissionDenied,
            format!(
                "`{collection}` has no column `{column}`, and an append-only insert \
                 cannot add one"
            ),
        )),
        None => Ok(()),
    }
}

/// Decode an `op` request and authorize it on the one resource `resource`
/// names, for every access [`op_checks`] lists — the typed request is
/// returned only when every check passed.
fn decode_and_authorize_op<T>(
    ctx: &dyn Context,
    body: &[u8],
    op: &str,
    resource: impl FnOnce(&T) -> &str,
) -> Result<T, OutputStream>
where
    T: serde::de::DeserializeOwned,
{
    decode_and_authorize_all(ctx, body, op, |req| op_checks(op, resource(req)))
}

/// Handle a database message using the given service.
///
/// `ctx` is the trusted host-side authorization surface: every op arm that
/// touches a WRAP-governed resource authorizes via
/// [`decode_and_authorize_op`] (or, for `database.batch`, per write via
/// [`decode_and_authorize_all`]), which bundles the codec decode with the
/// `ctx.check_resource_access` calls the op's
/// [`wafer_block::wrap::DATABASE_OP_ACCESS`] entry names, so an arm cannot
/// obtain its typed request without also being checked.
pub async fn handle_message(
    service: &dyn DatabaseService,
    ctx: &dyn Context,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::DATABASE_GET => {
            let req = match decode_and_authorize_op::<wire::GetRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_GET,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.get(&req.collection, &req.id).await {
                Ok(record) => to_output(service_record_to_wire(record)),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_LIST => {
            let req = match decode_and_authorize_op::<wire::ListRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_LIST,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let tree = match convert_filter_tree(req.filters) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            let sort = match convert_sort(req.sort) {
                Ok(s) => s,
                Err(e) => return OutputStream::error(e),
            };
            if matches!(&req.columns, Some(c) if c.is_empty()) {
                return OutputStream::error(invalid("columns must be non-empty when specified"));
            }
            if let Some(Err(e)) = req
                .columns
                .as_ref()
                .map(|c| c.iter().try_for_each(|c| check_name(c)))
            {
                return OutputStream::error(e);
            }
            // All LIST filtering — flat or group — flows through
            // `filter_tree`; `DbExec::list` renders it via
            // `query::build_condition_tree` as the `extra_condition` AND-ed
            // onto the (now-always-empty) flat `filters` clause. `filters`
            // stays empty here rather than the flattened leaves: keeping both
            // populated would double-apply flat predicates (once via
            // `opts.filters`, once via the `filter_tree` leaves already
            // covering them).
            let opts = ListOptions {
                filters: Vec::new(),
                sort,
                limit: req.limit,
                offset: req.offset,
                skip_count: req.skip_count,
                filter_tree: Some(tree),
                columns: req.columns,
            };
            match service.list(&req.collection, &opts).await {
                Ok(list) => to_output(service_record_list_to_wire(list)),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_CREATE => {
            let req = match decode_and_authorize_op::<wire::CreateRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_CREATE,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            if let Err(e) = check_data_names(&req.data) {
                return OutputStream::error(e);
            }
            if inserts_append_only(ctx, &req.collection) {
                if let Err(e) = check_append_only_rows(service, &req.collection, [&req.data]).await
                {
                    return OutputStream::error(e);
                }
            }
            match service.create(&req.collection, req.data).await {
                Ok(record) => to_output(service_record_to_wire(record)),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_CREATE_MANY => {
            let req = match decode_and_authorize_op::<wire::CreateManyRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_CREATE_MANY,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            // One INSERT per row: refused before anything runs when the
            // backend cannot run that many in this invocation.
            if let Err(e) = service
                .statement_budget()
                .and_then(|budget| budget.admit(req.rows.len(), "create_many"))
            {
                return OutputStream::error(db_error_to_wafer(e));
            }
            if let Err(e) = req.rows.iter().try_for_each(check_data_names) {
                return OutputStream::error(e);
            }
            if inserts_append_only(ctx, &req.collection) {
                if let Err(e) = check_append_only_rows(service, &req.collection, &req.rows).await {
                    return OutputStream::error(e);
                }
            }
            match service.create_many(&req.collection, req.rows).await {
                Ok(rows_affected) => to_output(&wire::CreateManyResponse { rows_affected }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_BATCH => {
            // Every write's collection must pass `check_name`, and is then
            // authorized, for the access `BatchWrite::access` names, before
            // any op is otherwise validated or run, so a batch naming one
            // write the caller may not make never touches the service.
            let req = match decode_and_authorize_all::<wire::BatchRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_BATCH,
                |r| {
                    if database_op_access(ServiceOp::DATABASE_BATCH)
                        != Some(DatabaseOpAccess::PerWrite)
                    {
                        return Err(WaferError::new(
                            ErrorCode::Internal,
                            "BUG: DATABASE_OP_ACCESS does not classify database.batch per write",
                        ));
                    }
                    let mut checks: Vec<(String, ResourceType, ResourceAccess)> = Vec::new();
                    for op in &r.ops {
                        check_name(op.collection())?;
                        let check = (op.collection().to_string(), ResourceType::Db, op.access());
                        if !checks.contains(&check) {
                            checks.push(check);
                        }
                    }
                    Ok(checks)
                },
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            // At most one statement per op (a `DeleteWhere` is one however
            // many rows it matches), so this refuses, before the append-only
            // probes or the service run, a batch the backend cannot run in
            // this invocation.
            if let Err(e) = service
                .statement_budget()
                .and_then(|budget| budget.admit(req.ops.len(), "batch"))
            {
                return OutputStream::error(db_error_to_wafer(e));
            }
            // A `Create` into a collection the caller may only append to
            // follows the append-only insert rules.
            let mut append_only: Vec<&str> = Vec::new();
            for op in &req.ops {
                if let wire::BatchWrite::Create { collection, .. } = op {
                    if !append_only.contains(&collection.as_str())
                        && inserts_append_only(ctx, collection)
                    {
                        append_only.push(collection);
                    }
                }
            }
            for collection in append_only {
                let rows = req.ops.iter().filter_map(|op| match op {
                    wire::BatchWrite::Create {
                        collection: c,
                        data,
                    } if c == collection => Some(data),
                    _ => None,
                });
                if let Err(e) = check_append_only_rows(service, collection, rows).await {
                    return OutputStream::error(e);
                }
            }
            let ops = match req
                .ops
                .into_iter()
                .map(to_write_op)
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(ops) => ops,
                Err(e) => return OutputStream::error(e),
            };
            match service.batch(ops).await {
                Ok(outcomes) => to_output(&wire::BatchResponse {
                    results: outcomes.into_iter().map(write_outcome_to_wire).collect(),
                }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_INSERT_GUARDED => {
            let req = match decode_and_authorize_op::<wire::InsertGuardedRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_INSERT_GUARDED,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let guards = match to_cap_guards(req.guards) {
                Ok(g) => g,
                Err(e) => return OutputStream::error(e),
            };
            if let Err(e) = check_data_names(&req.data) {
                return OutputStream::error(e);
            }
            if inserts_append_only(ctx, &req.collection) {
                if let Err(e) = check_append_only_rows(service, &req.collection, [&req.data]).await
                {
                    return OutputStream::error(e);
                }
            }
            match service
                .insert_guarded(&req.collection, req.data, &guards)
                .await
            {
                Ok(service::GuardedInsert::Inserted(record)) => {
                    to_output(&wire::InsertGuardedResponse::Inserted {
                        record: service_record_to_wire(record),
                    })
                }
                Ok(service::GuardedInsert::Refused { guard }) => {
                    to_output(&wire::InsertGuardedResponse::Refused { guard })
                }
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_UPDATE_GUARDED => {
            let req = match decode_and_authorize_op::<wire::UpdateGuardedRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_UPDATE_GUARDED,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let filters = match convert_filter_tree(req.filters).and_then(|t| flatten_leaves(&t)) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            let guards = match to_cap_guards(req.guards) {
                Ok(g) => g,
                Err(e) => return OutputStream::error(e),
            };
            if let Err(e) = check_data_names(&req.data) {
                return OutputStream::error(e);
            }
            match service
                .update_guarded(&req.collection, &filters, req.data, &guards)
                .await
            {
                Ok(outcome) => to_output(&match outcome {
                    service::GuardedUpdate::Updated { rows_affected } => {
                        wire::UpdateGuardedResponse::Updated { rows_affected }
                    }
                    service::GuardedUpdate::Refused { guard } => {
                        wire::UpdateGuardedResponse::Refused { guard }
                    }
                    service::GuardedUpdate::NoMatch => wire::UpdateGuardedResponse::NoMatch,
                }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_UPDATE => {
            let req = match decode_and_authorize_op::<wire::UpdateRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_UPDATE,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            if let Err(e) = check_data_names(&req.data) {
                return OutputStream::error(e);
            }
            match service.update(&req.collection, &req.id, req.data).await {
                Ok(record) => to_output(service_record_to_wire(record)),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DELETE => {
            let req = match decode_and_authorize_op::<wire::DeleteRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_DELETE,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.delete(&req.collection, &req.id).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_COUNT => {
            let req = match decode_and_authorize_op::<wire::CountRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_COUNT,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let tree = match convert_filter_tree(req.filters) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            let filters = match flatten_leaves(&tree) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            match service.count(&req.collection, &filters).await {
                Ok(count) => to_output(&wire::CountResponse { count }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_QUERY_RAW => {
            let req = match decode_and_authorize_op::<wire::QueryRawRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_QUERY_RAW,
                |_r| RAW_SQL_RESOURCE,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
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
            let req = match decode_and_authorize_op::<wire::SumRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_SUM,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let tree = match convert_filter_tree(req.filters) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            let filters = match flatten_leaves(&tree) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            if let Err(e) = check_name(&req.field) {
                return OutputStream::error(e);
            }
            match service.sum(&req.collection, &req.field, &filters).await {
                Ok(sum) => to_output(&wire::SumResponse { sum }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_EXEC_RAW => {
            let req = match decode_and_authorize_op::<wire::ExecRawRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_EXEC_RAW,
                |_r| RAW_SQL_RESOURCE,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.exec_raw(&req.query, &req.args).await {
                Ok(rows) => to_output(&wire::ExecRawResponse {
                    rows_affected: rows,
                }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DDL => {
            // Host-authoritative DDL sentinel (distinct op from
            // `DATABASE_EXEC_RAW` so a caller can't relabel a DDL statement
            // as a plain exec_raw, or vice versa, to dodge the `__ddl__`
            // resource check).
            let req = match decode_and_authorize_op::<wire::ExecRawRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_DDL,
                |_r| DDL_RESOURCE,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.exec_raw(&req.query, &req.args).await {
                Ok(rows) => to_output(&wire::ExecRawResponse {
                    rows_affected: rows,
                }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DELETE_WHERE => {
            let req = match decode_and_authorize_op::<wire::DeleteWhereRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_DELETE_WHERE,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let tree = match convert_filter_tree(req.filters) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            let filters = match flatten_leaves(&tree) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            match service.delete_where(&req.collection, &filters).await {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DELETE_WHERE_COUNT => {
            let req = match decode_and_authorize_op::<wire::DeleteWhereCountRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_DELETE_WHERE_COUNT,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let tree = match convert_filter_tree(req.filters) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            let filters = match flatten_leaves(&tree) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            match service.delete_where_count(&req.collection, &filters).await {
                Ok(count) => to_output(&wire::DeleteWhereCountResponse { count }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_TAKE_WHERE => {
            let req = match decode_and_authorize_op::<wire::TakeWhereRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_TAKE_WHERE,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let tree = match convert_filter_tree(req.filters) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            let filters = match flatten_leaves(&tree) {
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
            let req = match decode_and_authorize_op::<wire::UpdateWhereRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_UPDATE_WHERE,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let tree = match convert_filter_tree(req.filters) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            let filters = match flatten_leaves(&tree) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            if let Err(e) = check_data_names(&req.data) {
                return OutputStream::error(e);
            }
            match service
                .update_where(&req.collection, &filters, req.data)
                .await
            {
                Ok(()) => OutputStream::respond(vec![]),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_UPDATE_WHERE_COUNT => {
            let req = match decode_and_authorize_op::<wire::UpdateWhereCountRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_UPDATE_WHERE_COUNT,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let tree = match convert_filter_tree(req.filters) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            let filters = match flatten_leaves(&tree) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            if let Err(e) = check_data_names(&req.data) {
                return OutputStream::error(e);
            }
            match service
                .update_where_count(&req.collection, &filters, req.data)
                .await
            {
                Ok(count) => to_output(&wire::UpdateWhereCountResponse { count }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_INCREMENT_FIELD_WHERE => {
            let req = match decode_and_authorize_op::<wire::IncrementFieldWhereRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_INCREMENT_FIELD_WHERE,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let tree = match convert_filter_tree(req.filters) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            let filters = match flatten_leaves(&tree) {
                Ok(f) => f,
                Err(e) => return OutputStream::error(e),
            };
            if let Err(e) = check_name(&req.col) {
                return OutputStream::error(e);
            }
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
        ServiceOp::DATABASE_UPSERT => {
            let req = match decode_and_authorize_op::<wire::UpsertRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_UPSERT,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            let (collection, spec) = match to_upsert_spec(req) {
                Ok(pair) => pair,
                Err(e) => return OutputStream::error(e),
            };
            match service.upsert(&collection, spec).await {
                Ok(rows_affected) => to_output(&wire::UpsertResponse { rows_affected }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_AGGREGATE => {
            let req = match decode_and_authorize_op::<wire::AggregateRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_AGGREGATE,
                |r| &r.collection,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            if req.aggregates.is_empty() {
                return OutputStream::error(invalid(
                    "aggregate requires at least one aggregate column",
                ));
            }
            let (collection, spec) = match to_aggregate_spec(req) {
                Ok(pair) => pair,
                Err(e) => return OutputStream::error(e),
            };
            match service.aggregate(&collection, spec).await {
                Ok(records) => {
                    let wire_records: Vec<wire::Record> =
                        records.into_iter().map(service_record_to_wire).collect();
                    to_output(&wire_records)
                }
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_ENSURE_TABLE => {
            let req = match decode_and_authorize_op::<wire::EnsureTableRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_ENSURE_TABLE,
                |r| &r.table.name,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            // `SCHEMA_RESOURCE`, not `DDL_RESOURCE`: the statement is built
            // host-side from the validated `TableDef`, so this op does not
            // imply the arbitrary-statement `database.ddl` channel.
            if let Err(e) =
                ctx.check_resource_access(SCHEMA_RESOURCE, ResourceType::Db, ResourceAccess::Write)
            {
                return OutputStream::error(e);
            }
            let table = match schema_wire::table_from_def(&req.table) {
                Ok(t) => t,
                Err(e) => return OutputStream::error(e),
            };
            match service.ensure_schema_table(&table).await {
                Ok(()) => to_output(&wire::SchemaOpResponse {
                    table: req.table.name,
                }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_ADD_COLUMN => {
            let req = match decode_and_authorize_op::<wire::AddColumnRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_ADD_COLUMN,
                |r| &r.table,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            if let Err(e) =
                ctx.check_resource_access(SCHEMA_RESOURCE, ResourceType::Db, ResourceAccess::Write)
            {
                return OutputStream::error(e);
            }
            let column = match schema_wire::column_from_def(&req.column) {
                Ok(c) => c,
                Err(e) => return OutputStream::error(e),
            };
            match service.schema_add_column(&req.table, &column).await {
                Ok(()) => to_output(&wire::SchemaOpResponse { table: req.table }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_DROP_TABLE => {
            let req = match decode_and_authorize_op::<wire::DropTableRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_DROP_TABLE,
                |r| &r.table,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            if let Err(e) =
                ctx.check_resource_access(SCHEMA_RESOURCE, ResourceType::Db, ResourceAccess::Write)
            {
                return OutputStream::error(e);
            }
            match service.schema_drop_table(&req.table).await {
                Ok(()) => to_output(&wire::SchemaOpResponse { table: req.table }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        ServiceOp::DATABASE_TABLE_EXISTS => {
            let req = match decode_and_authorize_op::<wire::TableExistsRequest>(
                ctx,
                body,
                ServiceOp::DATABASE_TABLE_EXISTS,
                |r| &r.table,
            ) {
                Ok(r) => r,
                Err(out) => return out,
            };
            match service.schema_table_exists(&req.table).await {
                Ok(exists) => to_output(&wire::TableExistsResponse {
                    table: req.table,
                    exists,
                }),
                Err(e) => OutputStream::error(db_error_to_wafer(e)),
            }
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("unknown database operation: {other}"),
        )),
    }
}

/// Handle database lifecycle events (config application + schema migration on
/// Init).
///
/// `strict_schema` is the STRICT_SCHEMA flag the calling block read from its
/// own `lifecycle(Init)` config (see [`strict_schema_from`]); it is applied
/// here, before any migration or query, so the backend stores it off the
/// per-call hot path.
pub async fn handle_lifecycle(
    service: &dyn DatabaseService,
    tables: &[Table],
    strict_schema: bool,
    event: &LifecycleEvent,
) -> std::result::Result<(), WaferError> {
    if event.event_type == LifecycleType::Init {
        service.set_strict_schema(strict_schema);
        if strict_schema {
            tracing::info!("database STRICT_SCHEMA enabled — schema introspection disabled");
        }

        if tables.is_empty() {
            tracing::debug!("no schema tables configured — skipping migration");
        } else {
            // A transient fault keeps its `Unavailable` code, so the runtime
            // retries this Init rather than caching the failure.
            service
                .ensure_schema_tables(tables)
                .await
                .map_err(|e| WaferError::new(e.code(), format!("schema migration failed: {e}")))?;
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
        assert_eq!(w.code, ErrorCode::Internal);
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
    fn unavailable_is_transient_and_scrubbed() {
        let w = db_error_to_wafer(DatabaseError::Unavailable(
            "connect to db.internal:5432: connection refused".into(),
        ));
        assert_eq!(w.code, ErrorCode::Unavailable);
        assert_eq!(w.message, "database temporarily unavailable");
    }

    #[test]
    fn not_found_stays_descriptive() {
        let w = db_error_to_wafer(DatabaseError::NotFound);
        assert_eq!(w.code, ErrorCode::NotFound);
        assert_eq!(w.message, "record not found");
    }

    // `FilterOp::parse_wire` unit tests live next to the parser in
    // `wafer-block/src/db.rs`; the handler's use of it (including bad-operator
    // rejection) is covered by `filter_tree_conversion_tests` below.
}

#[cfg(test)]
mod filter_tree_conversion_tests {
    use wafer_block::wire::database::{FilterDef, FilterNode};

    use super::{convert_filter_tree, flatten_leaves, MAX_FILTER_DEPTH, MAX_FILTER_NODES};

    fn leaf(field: &str) -> FilterNode {
        FilterNode::Leaf(FilterDef {
            field: field.into(),
            operator: "eq".into(),
            value: serde_json::json!(1),
            column: None,
        })
    }

    #[test]
    fn flat_leaves_convert() {
        let tree = convert_filter_tree(vec![leaf("a"), leaf("b")]).unwrap();
        assert_eq!(tree.len(), 2);
        let flat = flatten_leaves(&tree).unwrap();
        assert_eq!(flat.len(), 2);
    }

    #[test]
    fn top_level_empty_is_ok() {
        // The top-level empty filter list means "no filter" and must stay
        // valid — only nested empty groups are rejected.
        let tree = convert_filter_tree(vec![]).unwrap();
        assert!(tree.is_empty());
        assert!(flatten_leaves(&tree).unwrap().is_empty());
    }

    #[test]
    fn group_is_rejected_by_flatten() {
        let tree = convert_filter_tree(vec![FilterNode::Any {
            any: vec![leaf("a")],
        }])
        .unwrap();
        let err = flatten_leaves(&tree).unwrap_err();
        assert_eq!(err.code, wafer_block::ErrorCode::InvalidArgument);
    }

    #[test]
    fn empty_group_is_rejected() {
        // A nested empty `all`/`any` group would otherwise convert to a
        // degenerate empty `Cond`; reject it so conversion stays fail-closed.
        let err = convert_filter_tree(vec![FilterNode::All { all: vec![] }]).unwrap_err();
        assert_eq!(err.code, wafer_block::ErrorCode::InvalidArgument);
        let err = convert_filter_tree(vec![FilterNode::Any { any: vec![] }]).unwrap_err();
        assert_eq!(err.code, wafer_block::ErrorCode::InvalidArgument);
    }

    #[test]
    fn depth_over_limit_is_rejected() {
        // Nest All groups MAX_FILTER_DEPTH+1 deep.
        let mut node = leaf("a");
        for _ in 0..(MAX_FILTER_DEPTH + 1) {
            node = FilterNode::All { all: vec![node] };
        }
        let err = convert_filter_tree(vec![node]).unwrap_err();
        assert_eq!(err.code, wafer_block::ErrorCode::InvalidArgument);
    }

    #[test]
    fn node_count_over_limit_is_rejected() {
        let many: Vec<FilterNode> = (0..(MAX_FILTER_NODES + 1)).map(|_| leaf("a")).collect();
        let err = convert_filter_tree(vec![FilterNode::All { all: many }]).unwrap_err();
        assert_eq!(err.code, wafer_block::ErrorCode::InvalidArgument);
    }

    #[test]
    fn bad_operator_is_rejected() {
        let bad = FilterNode::Leaf(FilterDef {
            field: "a".into(),
            operator: "no_such_op".into(),
            value: serde_json::json!(1),
            column: None,
        });
        let err = convert_filter_tree(vec![bad]).unwrap_err();
        assert_eq!(err.code, wafer_block::ErrorCode::InvalidArgument);
    }
}
