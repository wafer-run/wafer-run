//! [`forward_database_service!`] — write a [`DatabaseService`] impl as an
//! explicit ledger of what happens to every operation.
//!
//! [`DatabaseService`]: super::service::DatabaseService
//!
//! # The problem
//!
//! Eight of the trait's operations carry defaults, and none of them is a
//! pass-through:
//!
//! | operation | default |
//! |---|---|
//! | `delete_where` | `list` then `delete` per row, in a loop |
//! | `delete_where_count` | `count` then `delete_where` (a TOCTOU window) |
//! | `take_where` | `list` then `delete` per id — not atomic |
//! | `update_where` | `list` then `update` per row |
//! | `update_where_count` | `count` then `update_where` |
//! | `increment_field_where` | a hard `Internal` error |
//! | `ensure_schema_tables` | loop over `ensure_schema_table` |
//! | `set_strict_schema` | a silent no-op |
//!
//! Those defaults exist for backends that genuinely cannot express the bulk
//! statement. They are the wrong answer for a **decorator** — a cache, an
//! auditor, a guard — wrapping a backend that *can*: the decorator that omits
//! `take_where` does not pass the call through, it quietly substitutes a
//! list-then-delete against its own possibly-stale view and drops the wrapped
//! backend's atomic `DELETE … RETURNING *`. This has already happened once in a
//! consumer of this crate, and it is invisible in review because the bug is the
//! *absence* of code.
//!
//! # The shape of the fix
//!
//! The macro takes a ledger naming **every** operation on the trait, each with
//! one of three modes:
//!
//! - `forward` — delegate to the forward target (below).
//! - `custom` — this impl writes the method itself, inside the same invocation.
//! - `inherit` — deliberately take the `DatabaseService` trait default.
//!
//! An incomplete ledger does not expand, so "I forgot `take_where`" stops being
//! representable; `inherit` still gets you the default, but only by writing the
//! word.
//!
//! # Forward targets
//!
//! - `forward_to DbExec;` — the implementor is a SQL backend that implements
//!   [`DbExec`](super::exec::DbExec); `forward` entries call the shared
//!   executor's default of the same name, qualified so they cannot recurse into
//!   the method being defined. `DbExec` provides eighteen of the operations;
//!   the other five must be `custom` or `inherit`.
//! - `forward_to <method>();` — the implementor is a decorator with an inherent
//!   `fn <method>(&self) -> &dyn DatabaseService` returning the wrapped
//!   service; `forward` entries call it.
//!
//! # Invocation
//!
//! The macro emits the whole `impl` block, including the
//! `#[wafer_async_trait]` attribute — an `async fn` produced by a `macro_rules!`
//! *inside* an already-attributed impl is not seen by `async_trait` and would
//! not compile.
//!
//! The invoking crate must depend on `wafer-block` and `serde_json`, which the
//! generated signatures name.
//!
//! ```
//! use std::{collections::HashMap, sync::Arc};
//!
//! use wafer_block::db::{Filter, ListOptions};
//! use wafer_core::interfaces::database::service::{
//!     AggregateSpec, Column, DatabaseError, DatabaseService, Record, RecordList, Table,
//!     UpsertSpec,
//! };
//!
//! struct ReadOnlyGuard {
//!     inner: Arc<dyn DatabaseService>,
//! }
//!
//! impl ReadOnlyGuard {
//!     fn inner_service(&self) -> &dyn DatabaseService {
//!         self.inner.as_ref()
//!     }
//!
//!     fn refuse(op: &str) -> DatabaseError {
//!         DatabaseError::Internal(format!("{op}: this database is read-only"))
//!     }
//! }
//!
//! wafer_core::forward_database_service! {
//!     impl DatabaseService for ReadOnlyGuard {
//!         forward_to inner_service();
//!
//!         ops {
//!             get: forward,
//!             list: forward,
//!             create: custom,
//!             update: forward,
//!             delete: forward,
//!             count: forward,
//!             sum: forward,
//!             query_raw: forward,
//!             exec_raw: forward,
//!             delete_where: forward,
//!             delete_where_count: forward,
//!             take_where: forward,
//!             update_where: forward,
//!             update_where_count: forward,
//!             increment_field_where: forward,
//!             upsert: forward,
//!             aggregate: forward,
//!             ensure_schema_table: forward,
//!             ensure_schema_tables: inherit,
//!             schema_table_exists: forward,
//!             schema_drop_table: forward,
//!             schema_add_column: forward,
//!             set_strict_schema: forward,
//!         }
//!
//!         async fn create(
//!             &self,
//!             _collection: &str,
//!             _data: HashMap<String, serde_json::Value>,
//!         ) -> Result<Record, DatabaseError> {
//!             Err(Self::refuse("create"))
//!         }
//!     }
//! }
//! ```
//!
//! Dropping one line from that ledger is a compile error rather than a silently
//! inherited default:
//!
//! ```compile_fail
//! # use std::sync::Arc;
//! # use wafer_core::interfaces::database::service::DatabaseService;
//! struct Decorator {
//!     inner: Arc<dyn DatabaseService>,
//! }
//!
//! impl Decorator {
//!     fn inner_service(&self) -> &dyn DatabaseService {
//!         self.inner.as_ref()
//!     }
//! }
//!
//! wafer_core::forward_database_service! {
//!     impl DatabaseService for Decorator {
//!         forward_to inner_service();
//!         ops {
//!             get: forward,
//!             list: forward,
//!         }
//!     }
//! }
//! ```

/// Implement [`DatabaseService`](super::service::DatabaseService) from an
/// explicit per-operation ledger. See the [module docs](self) for the modes,
/// the forward targets, and worked examples.
#[macro_export]
macro_rules! forward_database_service {
    // A SQL backend forwarding to its own `DbExec` defaults.
    (
        impl DatabaseService for $ty:ty {
            forward_to DbExec;
            $($rest:tt)*
        }
    ) => {
        $crate::__forward_database_ledger! {
            $ty,
            $crate::interfaces::database::exec::DbExec,
            ::core::convert::identity,
            $($rest)*
        }
    };

    // A decorator forwarding to the service its `$accessor` returns.
    (
        impl DatabaseService for $ty:ty {
            forward_to $accessor:ident ();
            $($rest:tt)*
        }
    ) => {
        $crate::__forward_database_ledger! {
            $ty,
            $crate::interfaces::database::service::DatabaseService,
            Self::$accessor,
            $($rest)*
        }
    };

    ($($rest:tt)*) => {
        ::core::compile_error!(
            "forward_database_service! takes\n\
             \x20   impl DatabaseService for <Type> {\n\
             \x20       forward_to DbExec;            // or: forward_to <accessor>();\n\
             \x20       ops { <every operation>: forward|custom|inherit, }\n\
             \x20       <the `custom` methods>\n\
             \x20   }"
        );
    };
}

/// Completeness gate of [`forward_database_service!`]: this arm names every
/// [`DatabaseService`](super::service::DatabaseService) operation literally, so
/// a ledger that omits, reorders or misspells one fails to match and falls
/// through to the diagnostic arm. It then hands the ledger to
/// [`__forward_database_step!`] as a work list.
#[doc(hidden)]
#[macro_export]
macro_rules! __forward_database_ledger {
    (
        $ty:ty, $target:path, $recv:path,
        ops {
            get: $m_get:ident,
            list: $m_list:ident,
            create: $m_create:ident,
            update: $m_update:ident,
            delete: $m_delete:ident,
            count: $m_count:ident,
            sum: $m_sum:ident,
            query_raw: $m_query_raw:ident,
            exec_raw: $m_exec_raw:ident,
            delete_where: $m_delete_where:ident,
            delete_where_count: $m_delete_where_count:ident,
            take_where: $m_take_where:ident,
            update_where: $m_update_where:ident,
            update_where_count: $m_update_where_count:ident,
            increment_field_where: $m_increment_field_where:ident,
            upsert: $m_upsert:ident,
            aggregate: $m_aggregate:ident,
            ensure_schema_table: $m_ensure_schema_table:ident,
            ensure_schema_tables: $m_ensure_schema_tables:ident,
            schema_table_exists: $m_schema_table_exists:ident,
            schema_drop_table: $m_schema_drop_table:ident,
            schema_add_column: $m_schema_add_column:ident,
            set_strict_schema: $m_set_strict_schema:ident $(,)?
        }
        $($custom:tt)*
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, { $($custom)* },
            [
                (get, $m_get)
                (list, $m_list)
                (create, $m_create)
                (update, $m_update)
                (delete, $m_delete)
                (count, $m_count)
                (sum, $m_sum)
                (query_raw, $m_query_raw)
                (exec_raw, $m_exec_raw)
                (delete_where, $m_delete_where)
                (delete_where_count, $m_delete_where_count)
                (take_where, $m_take_where)
                (update_where, $m_update_where)
                (update_where_count, $m_update_where_count)
                (increment_field_where, $m_increment_field_where)
                (upsert, $m_upsert)
                (aggregate, $m_aggregate)
                (ensure_schema_table, $m_ensure_schema_table)
                (ensure_schema_tables, $m_ensure_schema_tables)
                (schema_table_exists, $m_schema_table_exists)
                (schema_drop_table, $m_schema_drop_table)
                (schema_add_column, $m_schema_add_column)
                (set_strict_schema, $m_set_strict_schema)
            ],
            []
        );
    };

    ($ty:ty, $target:path, $recv:path, $($rest:tt)*) => {
        ::core::compile_error!(
            "forward_database_service!'s `ops { … }` ledger must name EVERY \
             DatabaseService operation exactly once, in this order, each as \
             `forward`, `custom` or `inherit`:\n\
             \x20   get, list, create, update, delete, count, sum, query_raw, exec_raw,\n\
             \x20   delete_where, delete_where_count, take_where, update_where,\n\
             \x20   update_where_count, increment_field_where, upsert, aggregate,\n\
             \x20   ensure_schema_table, ensure_schema_tables, schema_table_exists,\n\
             \x20   schema_drop_table, schema_add_column, set_strict_schema\n\
             The listing is the point: an operation left out of a decorator \
             silently inherits a non-pass-through trait default."
        );
    };
}

/// Work-list muncher behind [`forward_database_service!`]: consumes one ledger
/// entry per step, appending the generated method to an accumulator, and emits
/// the whole `impl` block in the final step.
///
/// The accumulator exists because the `#[wafer_async_trait]` attribute must see
/// real `async fn` items: an attribute macro applied to an `impl` whose body
/// still holds unexpanded `macro_rules!` invocations skips them, and the
/// `async fn`s they later expand to never get desugared.
#[doc(hidden)]
#[macro_export]
macro_rules! __forward_database_step {
    // Work list drained: emit the impl.
    (
        $ty:ty, $target:path, $recv:path, { $($custom:tt)* },
        [], [ $($acc:tt)* ]
    ) => {
        #[$crate::wafer_async_trait]
        impl $crate::interfaces::database::service::DatabaseService for $ty {
            $($acc)*
            $($custom)*
        }
    };

    // Stated as written by this impl — the method comes from `$custom`.
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ ($op:ident, custom) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ], [ $($acc)* ]
        );
    };

    // Stated as deliberately taking the `DatabaseService` trait default.
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ ($op:ident, inherit) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ], [ $($acc)* ]
        );
    };

    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (get, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn get(
                    &self,
                    collection: &str,
                    id: &str,
                ) -> ::core::result::Result<
                    $crate::interfaces::database::service::Record,
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::get($recv(self), collection, id).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (list, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn list(
                    &self,
                    collection: &str,
                    opts: &::wafer_block::db::ListOptions,
                ) -> ::core::result::Result<
                    $crate::interfaces::database::service::RecordList,
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::list($recv(self), collection, opts).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (create, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn create(
                    &self,
                    collection: &str,
                    data: ::std::collections::HashMap<::std::string::String, ::serde_json::Value>,
                ) -> ::core::result::Result<
                    $crate::interfaces::database::service::Record,
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::create($recv(self), collection, data).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (update, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn update(
                    &self,
                    collection: &str,
                    id: &str,
                    data: ::std::collections::HashMap<::std::string::String, ::serde_json::Value>,
                ) -> ::core::result::Result<
                    $crate::interfaces::database::service::Record,
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::update($recv(self), collection, id, data).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (delete, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn delete(
                    &self,
                    collection: &str,
                    id: &str,
                ) -> ::core::result::Result<
                    (),
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::delete($recv(self), collection, id).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (count, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn count(
                    &self,
                    collection: &str,
                    filters: &[::wafer_block::db::Filter],
                ) -> ::core::result::Result<
                    i64,
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::count($recv(self), collection, filters).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (sum, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn sum(
                    &self,
                    collection: &str,
                    field: &str,
                    filters: &[::wafer_block::db::Filter],
                ) -> ::core::result::Result<
                    f64,
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::sum($recv(self), collection, field, filters).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (query_raw, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn query_raw(
                    &self,
                    query: &str,
                    args: &[::serde_json::Value],
                ) -> ::core::result::Result<
                    ::std::vec::Vec<$crate::interfaces::database::service::Record>,
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::query_raw($recv(self), query, args).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (exec_raw, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn exec_raw(
                    &self,
                    query: &str,
                    args: &[::serde_json::Value],
                ) -> ::core::result::Result<
                    i64,
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::exec_raw($recv(self), query, args).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (delete_where, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn delete_where(
                    &self,
                    collection: &str,
                    filters: &[::wafer_block::db::Filter],
                ) -> ::core::result::Result<
                    (),
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::delete_where($recv(self), collection, filters).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (delete_where_count, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn delete_where_count(
                    &self,
                    collection: &str,
                    filters: &[::wafer_block::db::Filter],
                ) -> ::core::result::Result<
                    i64,
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::delete_where_count($recv(self), collection, filters).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (take_where, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn take_where(
                    &self,
                    collection: &str,
                    filters: &[::wafer_block::db::Filter],
                ) -> ::core::result::Result<
                    ::std::vec::Vec<$crate::interfaces::database::service::Record>,
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::take_where($recv(self), collection, filters).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (update_where, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn update_where(
                    &self,
                    collection: &str,
                    filters: &[::wafer_block::db::Filter],
                    data: ::std::collections::HashMap<::std::string::String, ::serde_json::Value>,
                ) -> ::core::result::Result<
                    (),
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::update_where($recv(self), collection, filters, data).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (update_where_count, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn update_where_count(
                    &self,
                    collection: &str,
                    filters: &[::wafer_block::db::Filter],
                    data: ::std::collections::HashMap<::std::string::String, ::serde_json::Value>,
                ) -> ::core::result::Result<
                    i64,
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::update_where_count($recv(self), collection, filters, data).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (increment_field_where, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn increment_field_where(
                    &self,
                    collection: &str,
                    col: &str,
                    delta: i64,
                    filters: &[::wafer_block::db::Filter],
                ) -> ::core::result::Result<
                    i64,
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::increment_field_where($recv(self), collection, col, delta, filters).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (upsert, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn upsert(
                    &self,
                    collection: &str,
                    spec: $crate::interfaces::database::service::UpsertSpec,
                ) -> ::core::result::Result<
                    i64,
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::upsert($recv(self), collection, spec).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (aggregate, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn aggregate(
                    &self,
                    collection: &str,
                    spec: $crate::interfaces::database::service::AggregateSpec,
                ) -> ::core::result::Result<
                    ::std::vec::Vec<$crate::interfaces::database::service::Record>,
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::aggregate($recv(self), collection, spec).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (ensure_schema_table, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn ensure_schema_table(
                    &self,
                    table: &$crate::interfaces::database::service::Table,
                ) -> ::core::result::Result<
                    (),
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::ensure_schema_table($recv(self), table).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (ensure_schema_tables, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn ensure_schema_tables(
                    &self,
                    tables: &[$crate::interfaces::database::service::Table],
                ) -> ::core::result::Result<
                    (),
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::ensure_schema_tables($recv(self), tables).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (schema_table_exists, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn schema_table_exists(
                    &self,
                    name: &str,
                ) -> ::core::result::Result<
                    bool,
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::schema_table_exists($recv(self), name).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (schema_drop_table, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn schema_drop_table(
                    &self,
                    name: &str,
                ) -> ::core::result::Result<
                    (),
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::schema_drop_table($recv(self), name).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (schema_add_column, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                async fn schema_add_column(
                    &self,
                    table: &str,
                    column: &$crate::interfaces::database::service::Column,
                ) -> ::core::result::Result<
                    (),
                    $crate::interfaces::database::service::DatabaseError,
                > {
                    <_ as $target>::schema_add_column($recv(self), table, column).await
                }
            ]
        );
    };
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ (set_strict_schema, forward) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        $crate::__forward_database_step!(
            $ty, $target, $recv, $custom, [ $($todo)* ],
            [
                $($acc)*
                fn set_strict_schema(&self, enabled: bool) {
                    <_ as $target>::set_strict_schema($recv(self), enabled);
                }
            ]
        );
    };

    // Anything else is a mode this macro does not define.
    (
        $ty:ty, $target:path, $recv:path, $custom:tt,
        [ ($op:ident, $mode:ident) $($todo:tt)* ], [ $($acc:tt)* ]
    ) => {
        ::core::compile_error!(::core::concat!(
            "forward_database_service!: unknown mode `",
            ::core::stringify!($mode),
            "` for operation `",
            ::core::stringify!($op),
            "`; expected `forward`, `custom` or `inherit`"
        ));
    };
}
