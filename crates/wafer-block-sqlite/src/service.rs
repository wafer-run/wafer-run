use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use base64ct::{Base64, Encoding};
use rusqlite::{types::Value as SqlValue, Connection, OpenFlags, Row, TransactionBehavior};
use wafer_block_macro::wafer_async_trait;
#[cfg(test)]
use wafer_core::interfaces::database::service::{pk, DataType};
use wafer_core::{
    forward_database_service,
    interfaces::database::{
        codec::{self, JsonColumns},
        exec::{DbExec, TxOp, TxResult},
        schema_cache::SchemaCache,
        service::{Column, DatabaseError, Record, Table},
    },
};
use wafer_sql_utils::{ddl, introspect, Backend};

use crate::worker::{ConnWorker, WORKER_GONE};

/// Read-only workers opened alongside the write connection for file-backed
/// databases. WAL journaling allows readers to run concurrently with the
/// writer and each other, so reads no longer queue behind writes (or behind
/// other reads) the way they did on the single shared mutex. Two is enough
/// to overlap a slow scan with point reads without multiplying page-cache
/// memory; in-memory databases cannot share state across connections and
/// use the write worker for everything.
const READ_WORKERS: usize = 2;

/// SQLite implementation of the DatabaseService.
///
/// A dedicated worker thread owns the write connection (see
/// [`ConnWorker`]); file-backed databases add [`READ_WORKERS`] read-only
/// worker connections that serve the fetch/scalar paths. Async callers only
/// await channel sends and replies — SQLite I/O never runs on an executor
/// thread (PERF-02).
pub struct SQLiteDatabaseService {
    write: ConnWorker,
    readers: Vec<ConnWorker>,
    next_reader: AtomicUsize,
    /// Memoized table-exists / column-list facts (see [`SchemaCache`]).
    /// Invalidated on every schema mutation this service performs.
    schema_cache: SchemaCache,
    /// STRICT_SCHEMA flag; applied once at lifecycle `Init` via
    /// [`DatabaseService::set_strict_schema`]. When set, the shared executor
    /// skips schema introspection entirely.
    strict_schema: AtomicBool,
}

/// Apply the standard connection PRAGMAs: WAL journaling, foreign-key
/// enforcement, and a 5s busy timeout. Failures are logged but non-fatal
/// so callers always get a usable service.
fn apply_pragmas(db: &Connection) {
    if let Err(e) = db.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA foreign_keys=ON;
         PRAGMA busy_timeout=5000;",
    ) {
        tracing::warn!(error = %e, "failed to set SQLite PRAGMAs — performance and safety may be degraded");
    }
}

impl SQLiteDatabaseService {
    /// Wrap an open `rusqlite::Connection`, applying the standard PRAGMAs
    /// and moving it onto a dedicated worker thread. No read pool: the
    /// connection's origin (file, memory, shared cache) is unknown here, so
    /// additional connections cannot be opened safely.
    pub(crate) fn new(db: Connection) -> Self {
        apply_pragmas(&db);
        Self {
            write: ConnWorker::spawn(db, "sqlite-write"),
            readers: Vec::new(),
            next_reader: AtomicUsize::new(0),
            schema_cache: SchemaCache::new(),
            strict_schema: AtomicBool::new(false),
        }
    }

    /// Open a SQLite database file at `path` (creating it if absent) and
    /// return a configured service. Used by a native application build to back the
    /// `wafer-run/sqlite` block with an on-disk DB.
    ///
    /// Alongside the write connection, [`READ_WORKERS`] read-only reader
    /// connections are opened so WAL-mode reads run concurrently with
    /// writes. A reader failing to open degrades to fewer/no readers (the
    /// write worker serves reads then) rather than failing the service,
    /// matching the PRAGMA warn-and-continue policy above.
    pub fn open(path: &str) -> Result<Self, DatabaseError> {
        let conn = Connection::open(path).map_err(|e| step_error("open database", &e))?;
        apply_pragmas(&conn);

        let mut readers = Vec::new();
        for i in 0..READ_WORKERS {
            // Read-only at the SQLite level: the fetch/scalar paths (including
            // admin QUERY_RAW) are read APIs, so a write statement smuggled
            // through them now fails loudly instead of silently mutating.
            match Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY
                    | OpenFlags::SQLITE_OPEN_URI
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            ) {
                Ok(rconn) => {
                    if let Err(e) = rconn.execute_batch("PRAGMA busy_timeout=5000;") {
                        tracing::warn!(error = %e, "failed to set reader busy_timeout");
                    }
                    readers.push(ConnWorker::spawn(rconn, &format!("sqlite-read-{i}")));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to open read-only sqlite reader; reads fall back to the write worker");
                    break;
                }
            }
        }

        Ok(Self {
            write: ConnWorker::spawn(conn, "sqlite-write"),
            readers,
            next_reader: AtomicUsize::new(0),
            schema_cache: SchemaCache::new(),
            strict_schema: AtomicBool::new(false),
        })
    }

    /// Open an in-memory SQLite database for tests and ephemeral
    /// workloads. The connection lives for the lifetime of the worker
    /// thread and is dropped with the service.
    pub fn open_in_memory() -> Result<Self, DatabaseError> {
        let conn =
            Connection::open_in_memory().map_err(|e| step_error("open in-memory database", &e))?;
        Ok(Self::new(conn))
    }

    /// Number of dedicated read-only reader connections this service opened
    /// (`0` for [`open_in_memory`](Self::open_in_memory) / [`new`](Self::new)).
    /// `pub` so external tests — in particular the file-backed conformance
    /// invocation in `tests/conformance.rs` — can assert they are actually
    /// exercising the read/write-split configuration rather than silently
    /// degrading to the single-connection one, which is exactly the gap that
    /// let the `take_where` read/write-path bug through undetected.
    pub fn reader_count(&self) -> usize {
        self.readers.len()
    }

    /// Run a job on the write worker (all statements with side effects, and
    /// anything that must observe its own prior writes in program order).
    async fn on_write<T, F>(&self, f: F) -> Result<T, DatabaseError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> T + Send + 'static,
    {
        self.write
            .run(f)
            .await
            .map_err(|()| DatabaseError::Internal(WORKER_GONE.to_string()))
    }

    /// Run a read job on the next reader (round-robin), falling back to the
    /// write worker when no readers exist (in-memory / wrapped connections).
    ///
    /// Read-your-writes stays intact: WAL readers always see the latest
    /// COMMITTED state, and by the time an async caller issues a follow-up
    /// read, its write job has already completed (the write's reply is what
    /// resumed the caller).
    async fn on_read<T, F>(&self, f: F) -> Result<T, DatabaseError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> T + Send + 'static,
    {
        if self.readers.is_empty() {
            return self.on_write(f).await;
        }
        let i = self.next_reader.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        self.readers[i]
            .run(f)
            .await
            .map_err(|()| DatabaseError::Internal(WORKER_GONE.to_string()))
    }

    fn row_to_record(row: &Row, json: &JsonColumns) -> rusqlite::Result<Record> {
        let column_count = row.as_ref().column_count();
        let mut data = HashMap::new();
        let mut id = String::new();

        for i in 0..column_count {
            let col_name = row.as_ref().column_name(i).unwrap_or("").to_string();
            let value = match row.get_ref(i) {
                Ok(rusqlite::types::ValueRef::Null) => serde_json::Value::Null,
                Ok(rusqlite::types::ValueRef::Integer(n)) => serde_json::Value::Number(n.into()),
                Ok(rusqlite::types::ValueRef::Real(f)) => serde_json::Number::from_f64(f)
                    .map_or(serde_json::Value::Null, serde_json::Value::Number),
                // The shared codec owns the JSON-in-TEXT policy so this backend,
                // the browser's sql.js adapter and Cloudflare D1 decode the same
                // column the same way: by the declared JSON columns the
                // executor passed, never by what the text looks like.
                Ok(rusqlite::types::ValueRef::Text(s)) => {
                    codec::decode_text(&col_name, &String::from_utf8_lossy(s), json)
                }
                Ok(rusqlite::types::ValueRef::Blob(b)) => {
                    serde_json::Value::String(Base64::encode_string(b))
                }
                Err(_) => serde_json::Value::Null,
            };

            if col_name == "id" {
                id = codec::record_id(&value);
            }

            data.insert(col_name, value);
        }

        Ok(Record { id, data })
    }

    /// Prepare `sql`, bind `sql_params`, and decode every row via
    /// [`row_to_record`](Self::row_to_record) with `json`'s columns. Shared by `run_fetch` (queued
    /// on a reader) and `run_execute_returning` (queued on the writer) — the
    /// two primitives differ only in which worker runs this closure, never in
    /// how rows decode, so the decode itself lives in one place.
    ///
    /// A statement-level failure (a mid-query `step()` error — `SQLITE_BUSY`,
    /// `SQLITE_READONLY`, a `RETURNING` write that violates a constraint, …)
    /// must propagate as `Err`, never be silently dropped: `row_to_record`
    /// itself is infallible (every per-column decode failure maps to JSON
    /// `null`, never `Err`), so an `Err` surfacing from this iterator can only
    /// be a statement failure, not a row-decode failure. Collecting into a
    /// `rusqlite::Result<Vec<Record>>` (instead of filter-mapping per row)
    /// makes that propagate instead of being logged and dropped.
    fn fetch_rows(
        db: &Connection,
        sql: &str,
        sql_params: &[SqlValue],
        json: &JsonColumns,
    ) -> Result<Vec<Record>, DatabaseError> {
        let mut prepared = db.prepare(sql).map_err(|e| statement_error(&e))?;
        let records = prepared
            .query_map(as_params(sql_params).as_slice(), |row| {
                Self::row_to_record(row, json)
            })
            .map_err(|e| statement_error(&e))?
            .collect::<rusqlite::Result<Vec<Record>>>()
            .map_err(|e| statement_error(&e))?;
        Ok(records)
    }
}

/// A failed statement as a [`DatabaseError`]: a primary- or unique-key
/// violation is [`DatabaseError::AlreadyExists`]; a busy or locked database
/// (`SQLITE_BUSY` once the busy timeout ran out, `SQLITE_LOCKED`) is
/// [`DatabaseError::Unavailable`], since the same statement can succeed once
/// the other connection lets go; anything else `Internal`.
fn statement_error(e: &rusqlite::Error) -> DatabaseError {
    let rusqlite::Error::SqliteFailure(err, _) = e else {
        return DatabaseError::Internal(e.to_string());
    };
    if err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
        || err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
    {
        DatabaseError::AlreadyExists(e.to_string())
    } else if matches!(
        err.code,
        rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
    ) {
        DatabaseError::Unavailable(e.to_string())
    } else {
        DatabaseError::Internal(e.to_string())
    }
}

/// [`statement_error`] for a step that is not a caller's statement, with
/// `what` naming the step in the message.
fn step_error(what: &str, e: &rusqlite::Error) -> DatabaseError {
    match statement_error(e) {
        DatabaseError::Unavailable(msg) => DatabaseError::Unavailable(format!("{what}: {msg}")),
        DatabaseError::AlreadyExists(msg) | DatabaseError::Internal(msg) => {
            DatabaseError::Internal(format!("{what}: {msg}"))
        }
        other => other,
    }
}

fn json_to_sql_value(v: &serde_json::Value) -> SqlValue {
    match v {
        serde_json::Value::Null => SqlValue::Null,
        serde_json::Value::Bool(b) => SqlValue::Integer(if *b { 1 } else { 0 }),
        serde_json::Value::Number(n) => n.as_i64().map_or_else(
            || {
                n.as_f64()
                    .map_or_else(|| SqlValue::Text(n.to_string()), SqlValue::Real)
            },
            SqlValue::Integer,
        ),
        serde_json::Value::String(s) => SqlValue::Text(s.clone()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => SqlValue::Text(v.to_string()),
    }
}

/// Get list of column names for an existing table.
///
/// Synchronous sibling of the shared `DbExec::get_columns` default, for
/// callers that already hold the connection lock (`ensure_schema_table`).
/// Propagates real DB errors as [`DatabaseError`]; a row that fails to decode
/// is a real error too (the introspection shape is fixed), so we surface it
/// rather than silently dropping the column from the set.
fn table_columns(db: &Connection, table: &str) -> Result<Vec<String>, DatabaseError> {
    let (sql, params) = introspect::build_list_columns(table, Backend::Sqlite);
    let bound: Vec<SqlValue> = params.iter().map(json_to_sql_value).collect();
    let bound_refs: Vec<&dyn rusqlite::types::ToSql> = bound
        .iter()
        .map(|v| v as &dyn rusqlite::types::ToSql)
        .collect();
    let mut stmt = db
        .prepare(&sql)
        .map_err(|e| step_error(&format!("prepare list_columns {table}"), &e))?;
    let mut cols = Vec::new();
    let rows = stmt
        .query_map(bound_refs.as_slice(), |row| row.get::<_, String>(0))
        .map_err(|e| step_error(&format!("query list_columns {table}"), &e))?;
    for row in rows {
        let name =
            row.map_err(|e| step_error(&format!("read list_columns row for {table}"), &e))?;
        cols.push(name.to_lowercase());
    }
    Ok(cols)
}

/// Check if the table's `id` column is INTEGER PRIMARY KEY (autoincrement).
fn has_integer_pk(db: &Connection, table: &str) -> bool {
    let Ok((sql, _)) = introspect::build_table_info(table, Backend::Sqlite) else {
        return false;
    };
    let Ok(mut stmt) = db.prepare(&sql) else {
        return false;
    };
    let result = stmt.query_map([], |row| {
        let name: String = row.get(1)?;
        let col_type: String = row.get(2)?;
        let pk: i32 = row.get(5)?;
        Ok((name, col_type, pk))
    });
    if let Ok(rows) = result {
        for r in rows.flatten() {
            if r.0.to_lowercase() == "id" && r.2 > 0 && r.1.to_uppercase().contains("INT") {
                return true;
            }
        }
    }
    false
}

/// Bind owned [`SqlValue`]s as a `ToSql` slice. Runs inside worker jobs —
/// the conversion from JSON happens on the async side, the borrow for the
/// rusqlite call happens on the worker.
fn as_params(sql_params: &[SqlValue]) -> Vec<&dyn rusqlite::types::ToSql> {
    sql_params
        .iter()
        .map(|v| v as &dyn rusqlite::types::ToSql)
        .collect()
}

#[wafer_async_trait]
impl DbExec for SQLiteDatabaseService {
    const BACKEND: Backend = Backend::Sqlite;

    fn schema_cache(&self) -> Option<&SchemaCache> {
        Some(&self.schema_cache)
    }

    fn strict_schema(&self) -> bool {
        self.strict_schema.load(Ordering::Relaxed)
    }

    async fn run_fetch(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        json: &JsonColumns,
    ) -> Result<Vec<Record>, DatabaseError> {
        let sql = sql.to_string();
        let sql_params: Vec<SqlValue> = params.iter().map(json_to_sql_value).collect();
        let json = json.clone();
        self.on_read(move |db| Self::fetch_rows(db, &sql, &sql_params, &json))
            .await?
    }

    async fn run_fetch_one(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        json: &JsonColumns,
    ) -> Result<Record, DatabaseError> {
        let sql = sql.to_string();
        let sql_params: Vec<SqlValue> = params.iter().map(json_to_sql_value).collect();
        let json = json.clone();
        self.on_read(move |db| {
            db.query_row(&sql, as_params(&sql_params).as_slice(), |row| {
                Self::row_to_record(row, &json)
            })
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => DatabaseError::NotFound,
                _ => statement_error(&e),
            })
        })
        .await?
    }

    async fn run_execute(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let sql = sql.to_string();
        let sql_params: Vec<SqlValue> = params.iter().map(json_to_sql_value).collect();
        self.on_write(move |db| {
            let rows = db
                .execute(&sql, as_params(&sql_params).as_slice())
                .map_err(|e| statement_error(&e))?;
            Ok(rows as i64)
        })
        .await?
    }

    /// Same decode as [`run_fetch`](Self::run_fetch) (via
    /// [`fetch_rows`](Self::fetch_rows)), but queued on the write worker: the
    /// statement (`… RETURNING`) has side effects, so it must run against the
    /// single writable connection, never a read-only reader.
    async fn run_execute_returning(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        json: &JsonColumns,
    ) -> Result<Vec<Record>, DatabaseError> {
        let sql = sql.to_string();
        let sql_params: Vec<SqlValue> = params.iter().map(json_to_sql_value).collect();
        let json = json.clone();
        self.on_write(move |db| Self::fetch_rows(db, &sql, &sql_params, &json))
            .await?
    }

    async fn run_scalar_i64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let sql = sql.to_string();
        let sql_params: Vec<SqlValue> = params.iter().map(json_to_sql_value).collect();
        self.on_read(move |db| {
            db.query_row(&sql, as_params(&sql_params).as_slice(), |row| row.get(0))
                .map_err(|e| statement_error(&e))
        })
        .await?
    }

    async fn run_scalar_f64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<f64, DatabaseError> {
        let sql = sql.to_string();
        let sql_params: Vec<SqlValue> = params.iter().map(json_to_sql_value).collect();
        self.on_read(move |db| {
            db.query_row(&sql, as_params(&sql_params).as_slice(), |row| row.get(0))
                .map_err(|e| statement_error(&e))
        })
        .await?
    }

    async fn dbx_table_exists(&self, table: &str) -> Result<bool, DatabaseError> {
        let (sql, params) = introspect::build_table_exists(table, Backend::Sqlite);
        Ok(self.run_scalar_i64(&sql, &params).await? > 0)
    }

    /// Job-spanning insert: `last_insert_rowid()` is only meaningful while no
    /// other insert can run on the connection, so one worker job covers both
    /// calls (jobs on the write worker are strictly sequential).
    async fn run_insert(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Option<i64>, DatabaseError> {
        let sql = sql.to_string();
        let sql_params: Vec<SqlValue> = params.iter().map(json_to_sql_value).collect();
        self.on_write(move |db| {
            db.execute(&sql, as_params(&sql_params).as_slice())
                .map_err(|e| statement_error(&e))?;
            Ok(Some(db.last_insert_rowid()))
        })
        .await?
    }

    /// One write-worker job: the transaction holds the only writable
    /// connection from `BEGIN IMMEDIATE` to `COMMIT`, so no other write
    /// interleaves with it, and returning early on a failed statement drops
    /// the uncommitted [`rusqlite::Transaction`], which rolls it back.
    async fn run_transaction(&self, ops: &[TxOp<'_>]) -> Result<Vec<TxResult>, DatabaseError> {
        // `Some(json)` for a statement whose rows are decoded, `None` for one
        // whose affected count is the result.
        let statements: Vec<(Option<JsonColumns>, String, Vec<SqlValue>)> = ops
            .iter()
            .map(|op| {
                let (sql, params) = op.sql_params();
                let returning = match op {
                    TxOp::Returning { json, .. } => Some((*json).clone()),
                    TxOp::Execute { .. } => None,
                };
                (
                    returning,
                    sql.to_string(),
                    params.iter().map(json_to_sql_value).collect(),
                )
            })
            .collect();
        self.on_write(move |db| {
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| step_error("begin transaction", &e))?;
            let mut results = Vec::with_capacity(statements.len());
            for (returning, sql, params) in &statements {
                let result = if let Some(json) = returning {
                    TxResult::Returning(Self::fetch_rows(&tx, sql, params, json)?)
                } else {
                    let rows = tx
                        .execute(sql, as_params(params).as_slice())
                        .map_err(|e| statement_error(&e))?;
                    TxResult::Execute(rows as i64)
                };
                results.push(result);
            }
            tx.commit()
                .map_err(|e| step_error("commit transaction", &e))?;
            Ok(results)
        })
        .await?
    }

    /// Tables with `INTEGER PRIMARY KEY` autoincrement generate their own id;
    /// `create` must not synthesize a UUID for them.
    async fn table_autogenerates_id(&self, table: &str) -> bool {
        let table = table.to_string();
        self.on_read(move |db| has_integer_pk(db, &table))
            .await
            .unwrap_or(false)
    }
}

impl SQLiteDatabaseService {
    /// The DDL sequence behind `ensure_schema_table`, run as ONE write-worker
    /// job so create/alter/index share a single continuous lock hold. That
    /// atomicity is the only reason this backend overrides the shared
    /// [`DbExec::ensure_schema_table`] default rather than inheriting it; the
    /// policy below is the same one.
    ///
    /// All SQL is built on the async side (no connection needed); only
    /// execution queues to the write worker.
    async fn ensure_schema_table_in_one_job(&self, table: &Table) -> Result<(), DatabaseError> {
        let table_name = table.name.clone();
        let create_sql = ddl::build_create_table(table, Backend::Sqlite)?.sql;
        // (lowercased name, display name, ALTER sql) per declared column.
        let column_adds: Vec<(String, String, String)> = table
            .columns
            .iter()
            .map(|col| {
                let add = ddl::build_add_column(&table.name, col, Backend::Sqlite)?;
                Ok((col.name.to_lowercase(), col.name.clone(), add.sql))
            })
            .collect::<Result<_, DatabaseError>>()?;
        let mut index_sqls = Vec::new();
        for idx in &table.indexes {
            index_sqls.push(ddl::build_create_index(&table.name, idx, Backend::Sqlite)?.sql);
        }
        let fk_sqls: Vec<String> = ddl::build_fk_indexes(table, Backend::Sqlite)?
            .into_iter()
            .map(|stmt| stmt.sql)
            .collect();

        self.on_write(move |db| {
            db.execute_batch(&create_sql)
                .map_err(|e| step_error(&format!("create table {table_name}"), &e))?;

            // Add any missing columns. The table was just created above, so a
            // failure to read its columns is a real error, not "no columns" —
            // propagate it (matches `table_columns`' fail-loud contract).
            let existing = table_columns(db, &table_name)?;
            for (lower, name, alter_sql) in &column_adds {
                if existing.contains(lower) {
                    continue;
                }
                if let Err(e) = db.execute_batch(alter_sql) {
                    // SQLite has no `ADD COLUMN IF NOT EXISTS`, so the read
                    // above and this ALTER are not atomic: a concurrent writer
                    // may have added the column in between, which is benign.
                    // Re-read the true column set to tell the two apart. A
                    // column that is still missing means the ALTER genuinely
                    // failed, and reporting that as success (which this used to
                    // do, with a `warn!`) claims a migration that did not
                    // happen — every later write against the column then fails
                    // with "no such column" instead.
                    if !table_columns(db, &table_name)?.contains(lower) {
                        return Err(step_error(
                            &format!("add column {name} to {table_name}"),
                            &e,
                        ));
                    }
                }
            }

            for sql in &index_sqls {
                db.execute_batch(sql)
                    .map_err(|e| step_error("create index", &e))?;
            }

            // Indexes for columns with foreign keys
            for sql in &fk_sqls {
                db.execute_batch(sql)
                    .map_err(|e| step_error("create FK index", &e))?;
            }
            Ok(())
        })
        .await?
    }
}

forward_database_service! {
    impl DatabaseService for SQLiteDatabaseService {
        forward_to DbExec;

        ops {
            get: forward,
            list: forward,
            create: forward,
            create_many: forward,
            update: forward,
            delete: forward,
            count: forward,
            sum: forward,
            query_raw: forward,
            exec_raw: forward,
            delete_where: forward,
            delete_where_count: forward,
            take_where: forward,
            update_where: forward,
            update_where_count: forward,
            increment_field_where: forward,
            upsert: forward,
            aggregate: forward,
            batch: forward,
            insert_guarded: forward,
            update_guarded: forward,
            // The four below are not `DbExec` operations: the shared executor
            // has no schema-mutation primitives, and STRICT_SCHEMA is per-backend
            // state.
            ensure_schema_table: custom,
            // The trait default loops over `ensure_schema_table`, which is
            // exactly right here — each table already gets its own write job.
            ensure_schema_tables: inherit,
            schema_table_exists: forward,
            schema_columns: forward,
            schema_drop_table: custom,
            schema_add_column: custom,
            set_strict_schema: custom,
        }

        async fn ensure_schema_table(&self, table: &Table) -> Result<(), DatabaseError> {
            let result = self.ensure_schema_table_in_one_job(table).await;
            // The migration created the table and/or added columns (or failed
            // partway) — drop any cached facts so the next introspection reads
            // the true schema. Invalidate before propagating the inner result.
            self.schema_cache.invalidate(&table.name);
            result
        }

        async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError> {
            let stmt = ddl::build_drop_table(name, Backend::Sqlite)?;
            self.run_execute(&stmt.sql, &[]).await?;
            self.schema_cache.invalidate(name);
            Ok(())
        }

        async fn schema_add_column(
            &self,
            table: &str,
            column: &Column,
        ) -> Result<(), DatabaseError> {
            let stmt = ddl::build_add_column(table, column, Backend::Sqlite)?;
            self.run_execute(&stmt.sql, &[]).await?;
            self.schema_cache.invalidate(table);
            Ok(())
        }

        fn set_strict_schema(&self, enabled: bool) {
            self.strict_schema.store(enabled, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use wafer_block::db::{Filter, FilterOp, FilterTree, ListOptions, SortField};
    use wafer_core::interfaces::database::service::DatabaseService;
    use wafer_sql_utils::value::sea_values_to_json;

    use super::*;

    #[test]
    fn busy_and_locked_are_unavailable_and_a_taken_key_already_exists() {
        let failure = |code: std::ffi::c_int| {
            rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
        };
        for code in [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_BUSY_SNAPSHOT,
            rusqlite::ffi::SQLITE_LOCKED,
        ] {
            assert!(
                matches!(
                    statement_error(&failure(code)),
                    DatabaseError::Unavailable(_)
                ),
                "code {code} is transient"
            );
        }
        assert!(matches!(
            statement_error(&failure(rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE)),
            DatabaseError::AlreadyExists(_)
        ));
        for code in [rusqlite::ffi::SQLITE_ERROR, rusqlite::ffi::SQLITE_CORRUPT] {
            assert!(
                matches!(statement_error(&failure(code)), DatabaseError::Internal(_)),
                "code {code} is not transient"
            );
        }
        assert!(matches!(
            step_error("begin transaction", &failure(rusqlite::ffi::SQLITE_BUSY)),
            DatabaseError::Unavailable(msg) if msg.starts_with("begin transaction: ")
        ));
    }

    // -----------------------------------------------------------------------
    // json_to_sql_value type conversion tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_json_to_sql_null() {
        assert_eq!(json_to_sql_value(&serde_json::Value::Null), SqlValue::Null);
    }

    #[test]
    fn test_json_to_sql_bool() {
        assert_eq!(
            json_to_sql_value(&serde_json::json!(true)),
            SqlValue::Integer(1)
        );
        assert_eq!(
            json_to_sql_value(&serde_json::json!(false)),
            SqlValue::Integer(0)
        );
    }

    #[test]
    fn test_json_to_sql_integer() {
        assert_eq!(
            json_to_sql_value(&serde_json::json!(42)),
            SqlValue::Integer(42)
        );
        assert_eq!(
            json_to_sql_value(&serde_json::json!(-7)),
            SqlValue::Integer(-7)
        );
    }

    #[test]
    fn test_json_to_sql_float() {
        assert_eq!(
            json_to_sql_value(&serde_json::json!(2.5)),
            SqlValue::Real(2.5)
        );
    }

    #[test]
    fn test_json_to_sql_string() {
        assert_eq!(
            json_to_sql_value(&serde_json::json!("hello")),
            SqlValue::Text("hello".to_string())
        );
    }

    #[test]
    fn test_json_to_sql_array() {
        let v = serde_json::json!([1, 2, 3]);
        match json_to_sql_value(&v) {
            SqlValue::Text(s) => assert_eq!(s, "[1,2,3]"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn test_json_to_sql_object() {
        let v = serde_json::json!({"key": "val"});
        match json_to_sql_value(&v) {
            SqlValue::Text(s) => {
                let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
                assert_eq!(parsed, serde_json::json!({"key": "val"}));
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Sea-query builder integration tests (SQLite dialect)
    // -----------------------------------------------------------------------

    #[test]
    fn test_sea_query_select_with_filters() {
        let opts = ListOptions {
            filters: vec![Filter {
                field: "name".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!("alice"),
            }],
            sort: vec![],
            limit: None,
            offset: 0,
            skip_count: false,
            filter_tree: None,
            columns: None,
        };
        let stmt = wafer_sql_utils::query::build_select("users", &opts, &["id"], Backend::Sqlite)
            .expect("renders");
        let sql = stmt.sql;
        assert!(sql.contains("WHERE"));
        // SQLite uses ? placeholders, not $N
        assert!(sql.contains("?"), "SQLite should use ? placeholders");
        assert!(!sql.contains("$1"), "SQLite should not use $N placeholders");
        let params = sea_values_to_json(stmt.values)
            .iter()
            .map(json_to_sql_value)
            .collect::<Vec<_>>();
        assert_eq!(params.len(), 1);
        assert_eq!(params[0], SqlValue::Text("alice".to_string()));
    }

    #[test]
    fn test_sea_query_select_with_sort_and_pagination() {
        let opts = ListOptions {
            filters: vec![],
            sort: vec![
                SortField {
                    field: "created_at".to_string(),
                    desc: true,
                },
                SortField {
                    field: "name".to_string(),
                    desc: false,
                },
            ],
            limit: Some(10),
            offset: 20,
            skip_count: false,
            filter_tree: None,
            columns: None,
        };
        let stmt = wafer_sql_utils::query::build_select("items", &opts, &["id"], Backend::Sqlite)
            .expect("renders");
        assert!(stmt.sql.contains("ORDER BY"));
        assert!(stmt.sql.contains("LIMIT"));
        assert!(stmt.sql.contains("OFFSET"));
    }

    #[test]
    fn test_sea_query_count_with_filters() {
        let filters = vec![Filter {
            field: "active".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(true),
        }];
        let stmt = wafer_sql_utils::aggregate::build_count("users", &filters, Backend::Sqlite);
        let sql = stmt.sql;
        assert!(sql.contains("COUNT(*)"));
        assert!(sql.contains("WHERE"));
        assert_eq!(sea_values_to_json(stmt.values).len(), 1);
    }

    #[test]
    fn test_sea_query_sum() {
        let filters = vec![Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("active"),
        }];
        let stmt =
            wafer_sql_utils::aggregate::build_sum("orders", "amount", &filters, Backend::Sqlite);
        let sql = stmt.sql;
        assert!(sql.contains("SUM"));
        assert!(sql.contains("COALESCE"));
        assert!(sql.contains("WHERE"));
        assert!(!sea_values_to_json(stmt.values).is_empty());
    }

    #[test]
    fn test_sea_query_delete_where() {
        let filters = vec![Filter {
            field: "status".to_string(),
            operator: FilterOp::In,
            value: serde_json::json!(["active", "pending"]),
        }];
        let stmt = wafer_sql_utils::query::build_delete_where("users", &filters, Backend::Sqlite);
        let sql = stmt.sql;
        assert!(sql.contains("DELETE FROM"));
        assert!(sql.contains("IN"));
        let params = sea_values_to_json(stmt.values)
            .iter()
            .map(json_to_sql_value)
            .collect::<Vec<_>>();
        assert_eq!(params.len(), 2);
        assert_eq!(params[0], SqlValue::Text("active".to_string()));
        assert_eq!(params[1], SqlValue::Text("pending".to_string()));
    }

    #[test]
    fn test_sea_query_update_where() {
        let data = vec![("status".to_string(), serde_json::json!("active"))];
        let filters = vec![Filter {
            field: "id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("123"),
        }];
        let stmt =
            wafer_sql_utils::query::build_update_where("users", &data, &filters, Backend::Sqlite);
        let sql = stmt.sql;
        assert!(sql.contains("UPDATE"));
        assert!(sql.contains("SET"));
        assert!(sql.contains("WHERE"));
        assert_eq!(sea_values_to_json(stmt.values).len(), 2);
    }

    #[test]
    fn test_sea_query_is_null_filter() {
        let filters = vec![Filter {
            field: "deleted_at".to_string(),
            operator: FilterOp::IsNull,
            value: serde_json::Value::Null,
        }];
        let stmt = wafer_sql_utils::query::build_delete_where("users", &filters, Backend::Sqlite);
        assert!(stmt.sql.contains("IS NULL"));
        assert!(sea_values_to_json(stmt.values).is_empty());
    }

    #[test]
    fn test_sea_query_is_not_null_filter() {
        let filters = vec![Filter {
            field: "email".to_string(),
            operator: FilterOp::IsNotNull,
            value: serde_json::Value::Null,
        }];
        let stmt = wafer_sql_utils::aggregate::build_count("users", &filters, Backend::Sqlite);
        assert!(stmt.sql.contains("IS NOT NULL"));
        assert!(sea_values_to_json(stmt.values).is_empty());
    }

    #[test]
    fn test_sea_query_like_filter() {
        let filters = vec![Filter {
            field: "name".to_string(),
            operator: FilterOp::Like,
            value: serde_json::json!("%alice%"),
        }];
        let stmt = wafer_sql_utils::aggregate::build_count("users", &filters, Backend::Sqlite);
        assert!(stmt.sql.contains("LIKE"));
    }

    #[test]
    fn test_sea_query_comparison_ops() {
        let filters = vec![
            Filter {
                field: "age".to_string(),
                operator: FilterOp::GreaterEqual,
                value: serde_json::json!(18),
            },
            Filter {
                field: "score".to_string(),
                operator: FilterOp::LessThan,
                value: serde_json::json!(100),
            },
        ];
        let stmt = wafer_sql_utils::aggregate::build_count("users", &filters, Backend::Sqlite);
        let sql = stmt.sql;
        assert!(sql.contains(">="));
        assert!(sql.contains("<"));
        assert_eq!(sea_values_to_json(stmt.values).len(), 2);
    }

    // -----------------------------------------------------------------------
    // Integration tests — delete_where_count + take_where (in-memory SQLite)
    // -----------------------------------------------------------------------

    fn make_test_svc() -> SQLiteDatabaseService {
        SQLiteDatabaseService::open_in_memory().unwrap()
    }

    /// Raw multi-statement setup executed on the write worker — the test-side
    /// replacement for the retired direct `svc.db.lock()` access.
    async fn exec_batch_for_tests(svc: &SQLiteDatabaseService, sql: &str) {
        let sql = sql.to_string();
        svc.on_write(move |db| db.execute_batch(&sql).unwrap())
            .await
            .unwrap();
    }

    async fn seed_rows(
        svc: &SQLiteDatabaseService,
        collection: &str,
        rows: Vec<serde_json::Value>,
    ) {
        // Declare a TEXT-everything schema from the union of row keys, then
        // create it via `ensure_schema_table` before inserting. The runtime
        // no longer auto-creates tables on first insert — production callers
        // run explicit migrations at `Init`, and the test fixture mirrors
        // that contract.
        let mut keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for row in &rows {
            if let serde_json::Value::Object(map) = row {
                for k in map.keys() {
                    if k != "id" && k != "created_at" && k != "updated_at" {
                        keys.insert(k.clone());
                    }
                }
            }
        }
        let mut columns = vec![pk("id")];
        for k in &keys {
            columns.push(Column::new(k, DataType::Text).null());
        }
        columns.push(Column::new("created_at", DataType::Text).null());
        columns.push(Column::new("updated_at", DataType::Text).null());
        let table = Table {
            name: collection.to_string(),
            columns,
            indexes: Vec::new(),
            primary_key: Vec::new(),
            unique_keys: Vec::new(),
        };
        DatabaseService::ensure_schema_table(svc, &table)
            .await
            .unwrap();

        for row in rows {
            let mut data = std::collections::HashMap::new();
            if let serde_json::Value::Object(map) = row {
                for (k, v) in map {
                    data.insert(k, v);
                }
            }
            DatabaseService::create(svc, collection, data)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn delete_where_count_returns_affected_row_count() {
        let svc = make_test_svc();
        seed_rows(
            &svc,
            "items",
            vec![
                serde_json::json!({"name": "alpha", "status": "active"}),
                serde_json::json!({"name": "beta", "status": "active"}),
                serde_json::json!({"name": "gamma", "status": "inactive"}),
            ],
        )
        .await;

        let filters = vec![Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("active"),
        }];

        let count = DatabaseService::delete_where_count(&svc, "items", &filters)
            .await
            .unwrap();
        assert_eq!(count, 2, "should have deleted exactly 2 active rows");

        // Remaining row is the inactive one
        let remaining = DatabaseService::count(&svc, "items", &[]).await.unwrap();
        assert_eq!(remaining, 1);
    }

    #[tokio::test]
    async fn delete_where_count_returns_zero_when_no_match() {
        let svc = make_test_svc();
        seed_rows(
            &svc,
            "items",
            vec![serde_json::json!({"name": "alpha", "status": "active"})],
        )
        .await;

        let filters = vec![Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("nonexistent"),
        }];

        let count = DatabaseService::delete_where_count(&svc, "items", &filters)
            .await
            .unwrap();
        assert_eq!(count, 0);

        // Row still exists
        let remaining = DatabaseService::count(&svc, "items", &[]).await.unwrap();
        assert_eq!(remaining, 1);
    }

    #[tokio::test]
    async fn delete_where_count_on_missing_table_returns_zero() {
        let svc = make_test_svc();
        let filters = vec![Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("active"),
        }];
        let count = DatabaseService::delete_where_count(&svc, "no_such_table", &filters)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn take_where_returns_deleted_rows() {
        let svc = make_test_svc();
        seed_rows(
            &svc,
            "codes",
            vec![
                serde_json::json!({"code": "abc123", "used": false}),
                serde_json::json!({"code": "xyz789", "used": false}),
                serde_json::json!({"code": "def456", "used": true}),
            ],
        )
        .await;

        let filters = vec![Filter {
            field: "used".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(false),
        }];

        let taken = DatabaseService::take_where(&svc, "codes", &filters)
            .await
            .unwrap();
        assert_eq!(taken.len(), 2, "should have taken 2 unused codes");

        // Verify the rows are actually deleted
        let remaining = DatabaseService::count(&svc, "codes", &[]).await.unwrap();
        assert_eq!(remaining, 1, "only 1 used code should remain");
    }

    #[tokio::test]
    async fn take_where_returns_empty_when_no_match() {
        let svc = make_test_svc();
        seed_rows(
            &svc,
            "codes",
            vec![serde_json::json!({"code": "abc123", "used": true})],
        )
        .await;

        let filters = vec![Filter {
            field: "used".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(false),
        }];

        let taken = DatabaseService::take_where(&svc, "codes", &filters)
            .await
            .unwrap();
        assert!(taken.is_empty());

        // Original row still present
        let remaining = DatabaseService::count(&svc, "codes", &[]).await.unwrap();
        assert_eq!(remaining, 1);
    }

    #[tokio::test]
    async fn take_where_on_missing_table_returns_empty() {
        let svc = make_test_svc();
        let filters = vec![Filter {
            field: "code".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("abc"),
        }];
        let taken = DatabaseService::take_where(&svc, "no_such_table", &filters)
            .await
            .unwrap();
        assert!(taken.is_empty());
    }

    /// B1 (PR #333 review): a statement-level failure on the write-returning
    /// path (`DELETE … RETURNING` violating a `FOREIGN KEY` constraint) must
    /// propagate as `Err`, never be swallowed into `Ok(vec![])`.
    /// `row_to_record` is infallible (every per-column decode failure maps to
    /// JSON `null`), so the only `Err` `query_map` can ever yield here is a
    /// statement failure — dropping it (as the pre-fix `filter_map` swallow
    /// did) reproduces the exact silent-data-loss shape this PR exists to
    /// close, just via a different trigger than the original read-only-path
    /// bug (works even on the in-memory service, no reader split needed: the
    /// failure is in the write connection's own FK enforcement, not in
    /// routing).
    #[tokio::test]
    async fn run_execute_returning_propagates_a_statement_level_failure() {
        let svc = make_test_svc();
        exec_batch_for_tests(
            &svc,
            "CREATE TABLE parent (id TEXT PRIMARY KEY);
             CREATE TABLE child (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT NOT NULL REFERENCES parent(id)
             );
             INSERT INTO parent (id) VALUES ('p1');
             INSERT INTO child (id, parent_id) VALUES ('c1', 'p1');",
        )
        .await;

        let err = svc
            .run_execute_returning(
                "DELETE FROM parent WHERE id = 'p1' RETURNING *",
                &[],
                JsonColumns::NONE,
            )
            .await
            .expect_err(
                "a DELETE that violates a FOREIGN KEY constraint must error, not return Ok([])",
            );
        assert!(
            err.to_string().to_lowercase().contains("foreign key"),
            "expected a foreign-key-constraint error, got: {err}"
        );

        // The parent row must still be present: the FK-violating DELETE never
        // applied. (True either way here since SQLite itself refused the
        // write — this assertion guards against a future change that starts
        // applying partial writes before the constraint check.)
        let remaining = DatabaseService::count(&svc, "parent", &[]).await.unwrap();
        assert_eq!(
            remaining, 1,
            "the FK-violating DELETE must not have executed"
        );
    }

    #[tokio::test]
    async fn increment_field_where_atomically_bumps_matching_rows() {
        let svc = make_test_svc();
        // access_count needs to be INTEGER for arithmetic; the TEXT-everything
        // helper seeds it as TEXT, so build the schema manually.
        let table = Table {
            name: "shares".into(),
            columns: vec![
                pk("id"),
                Column::new("access_count", DataType::Int).null(),
                Column::new("created_at", DataType::Text).null(),
                Column::new("updated_at", DataType::Text).null(),
            ],
            indexes: Vec::new(),
            primary_key: Vec::new(),
            unique_keys: Vec::new(),
        };
        DatabaseService::ensure_schema_table(&svc, &table)
            .await
            .unwrap();
        for id in ["a", "b", "c"] {
            let mut row = std::collections::HashMap::new();
            row.insert("id".into(), serde_json::json!(id));
            row.insert("access_count".into(), serde_json::json!(0));
            DatabaseService::create(&svc, "shares", row).await.unwrap();
        }

        // CAS-style bump on a single id with a max-cap predicate (the share.rs
        // pattern this op is built for).
        let filters = vec![
            Filter {
                field: "id".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!("a"),
            },
            Filter {
                field: "access_count".into(),
                operator: FilterOp::LessThan,
                value: serde_json::json!(5_i64),
            },
        ];
        let rows =
            DatabaseService::increment_field_where(&svc, "shares", "access_count", 1, &filters)
                .await
                .unwrap();
        assert_eq!(rows, 1, "exactly one row should match");

        let r = DatabaseService::get(&svc, "shares", "a").await.unwrap();
        assert_eq!(r.data["access_count"], serde_json::json!(1));
        let untouched = DatabaseService::get(&svc, "shares", "b").await.unwrap();
        assert_eq!(untouched.data["access_count"], serde_json::json!(0));
    }

    #[tokio::test]
    async fn increment_field_where_on_missing_table_returns_zero() {
        let svc = make_test_svc();
        let filters = vec![Filter {
            field: "id".into(),
            operator: FilterOp::Equal,
            value: serde_json::json!("nope"),
        }];
        let rows = DatabaseService::increment_field_where(
            &svc,
            "no_such_table",
            "access_count",
            1,
            &filters,
        )
        .await
        .unwrap();
        assert_eq!(rows, 0);
    }

    // -----------------------------------------------------------------------
    // upsert (INSERT … ON CONFLICT) — SetColumns + WindowedCounter
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn upsert_set_columns_inserts_then_updates_on_conflict() {
        use wafer_core::interfaces::database::service::{UpsertConflict, UpsertSpec};

        let svc = make_test_svc();
        let table = Table {
            name: "widgets".into(),
            columns: vec![pk("id"), Column::new("name", DataType::Text).null()],
            indexes: Vec::new(),
            primary_key: Vec::new(),
            unique_keys: Vec::new(),
        };
        DatabaseService::ensure_schema_table(&svc, &table)
            .await
            .unwrap();

        // No existing row on id=w1 → the ON CONFLICT insert lands as an insert.
        let n1 = DatabaseService::upsert(
            &svc,
            "widgets",
            UpsertSpec {
                data: vec![
                    ("id".into(), serde_json::json!("w1")),
                    ("name".into(), serde_json::json!("a")),
                ],
                conflict_columns: vec!["id".into()],
                on_conflict: UpsertConflict::SetColumns(vec!["name".into()]),
            },
        )
        .await
        .unwrap();
        assert_eq!(n1, 1, "insert affects one row");
        let r1 = DatabaseService::get(&svc, "widgets", "w1").await.unwrap();
        assert_eq!(r1.data["name"], serde_json::json!("a"));

        // Same id → conflict on the PK → DO UPDATE SET name = excluded.name.
        let n2 = DatabaseService::upsert(
            &svc,
            "widgets",
            UpsertSpec {
                data: vec![
                    ("id".into(), serde_json::json!("w1")),
                    ("name".into(), serde_json::json!("b")),
                ],
                conflict_columns: vec!["id".into()],
                on_conflict: UpsertConflict::SetColumns(vec!["name".into()]),
            },
        )
        .await
        .unwrap();
        assert_eq!(n2, 1, "conflict update affects one row");
        let r2 = DatabaseService::get(&svc, "widgets", "w1").await.unwrap();
        assert_eq!(
            r2.data["name"],
            serde_json::json!("b"),
            "on-conflict updated name a -> b"
        );

        let total = DatabaseService::count(&svc, "widgets", &[]).await.unwrap();
        assert_eq!(
            total, 1,
            "still exactly one row — the second call updated, not inserted"
        );
    }

    #[tokio::test]
    async fn upsert_windowed_counter_increments_in_window_and_keeps_created_at() {
        use wafer_core::interfaces::database::service::{UpsertConflict, UpsertSpec};

        let svc = make_test_svc();
        // Seed an existing counter row with SENTINEL timestamps so we can prove
        // created_at is immutable across conflict-updates (Task-5 fix): if the
        // builder wrongly re-stamped created_at in DO UPDATE SET, the sentinel
        // would be overwritten with CURRENT_TIMESTAMP. `key` is UNIQUE — the
        // conflict target.
        exec_batch_for_tests(
            &svc,
            "CREATE TABLE rl (
                 id TEXT PRIMARY KEY,
                 key TEXT UNIQUE,
                 count INTEGER,
                 window_start INTEGER,
                 created_at TEXT,
                 updated_at TEXT
             );
             INSERT INTO rl (id, key, count, window_start, created_at, updated_at)
             VALUES ('seed', 'user:1:login', 1, 1700000000, 'SENTINEL-CREATED', 'SENTINEL-UPDATED');",
        )
        .await;

        let now = 1_700_000_000_i64;
        let cutoff = now - 60; // 60s window; stored window_start (=now) is NOT expired
        let make_spec = || UpsertSpec {
            data: vec![
                ("id".into(), serde_json::json!("fresh-id")),
                ("key".into(), serde_json::json!("user:1:login")),
            ],
            conflict_columns: vec!["key".into()],
            on_conflict: UpsertConflict::WindowedCounter {
                count_field: "count".into(),
                window_field: "window_start".into(),
                now,
                window_cutoff: cutoff,
                created_fields: vec!["created_at".into()],
                updated_fields: vec!["updated_at".into()],
            },
        };

        // First upsert conflicts on `key` → in-window increment (1 -> 2).
        let n1 = DatabaseService::upsert(&svc, "rl", make_spec())
            .await
            .unwrap();
        assert_eq!(n1, 1, "conflict update affects the one matching row");
        let r1 = DatabaseService::get(&svc, "rl", "seed").await.unwrap();
        assert_eq!(
            r1.data["count"],
            serde_json::json!(2),
            "count incremented 1 -> 2"
        );
        assert_eq!(
            r1.data["created_at"],
            serde_json::json!("SENTINEL-CREATED"),
            "created_at must be immutable on conflict (Task-5 fix)"
        );
        assert_ne!(
            r1.data["updated_at"],
            serde_json::json!("SENTINEL-UPDATED"),
            "updated_at must be re-stamped on conflict"
        );

        // Second in-window upsert → increments again (2 -> 3); created_at still untouched.
        let n2 = DatabaseService::upsert(&svc, "rl", make_spec())
            .await
            .unwrap();
        assert_eq!(n2, 1);
        let r2 = DatabaseService::get(&svc, "rl", "seed").await.unwrap();
        assert_eq!(
            r2.data["count"],
            serde_json::json!(3),
            "count incremented 2 -> 3 on the second in-window upsert"
        );
        assert_eq!(
            r2.data["created_at"],
            serde_json::json!("SENTINEL-CREATED"),
            "created_at still unchanged after the second in-window upsert"
        );

        // The conflicting upserts never inserted a duplicate row.
        let total = DatabaseService::count(&svc, "rl", &[]).await.unwrap();
        assert_eq!(total, 1, "no duplicate row was inserted");
    }

    // -----------------------------------------------------------------------
    // aggregate (grouped queries) — count-by-column, case-when, date-bucket
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn aggregate_grouped_count_by_column_carries_alias_and_counts() {
        use wafer_core::interfaces::database::service::{
            AggregateColumnSpec, AggregateSpec, GroupBySpec,
        };

        let svc = make_test_svc();
        seed_rows(
            &svc,
            "items",
            vec![
                serde_json::json!({"status": "active"}),
                serde_json::json!({"status": "active"}),
                serde_json::json!({"status": "inactive"}),
            ],
        )
        .await;

        let spec = AggregateSpec {
            select_columns: vec!["status".into()],
            aggregates: vec![AggregateColumnSpec::Count {
                alias: "cnt".into(),
            }],
            filters: vec![],
            group_by: vec![GroupBySpec::Column("status".into())],
            sort: vec![SortField {
                field: "status".into(),
                desc: false,
            }],
            limit: 0,
        };
        let rows = DatabaseService::aggregate(&svc, "items", spec)
            .await
            .unwrap();

        assert_eq!(rows.len(), 2, "two distinct status groups");
        // Sorted ascending: active, inactive.
        assert_eq!(rows[0].data["status"], serde_json::json!("active"));
        assert_eq!(
            rows[0].data["cnt"],
            serde_json::json!(2),
            "alias carries count"
        );
        assert_eq!(rows[1].data["status"], serde_json::json!("inactive"));
        assert_eq!(rows[1].data["cnt"], serde_json::json!(1));
    }

    #[tokio::test]
    async fn aggregate_case_when_sum_counts_matching_rows_per_group() {
        use wafer_core::interfaces::database::service::{
            AggregateColumnSpec, AggregateSpec, GroupBySpec,
        };

        let svc = make_test_svc();
        seed_rows(
            &svc,
            "reqs",
            vec![
                serde_json::json!({"method": "GET", "status": "ok"}),
                serde_json::json!({"method": "GET", "status": "error"}),
                serde_json::json!({"method": "GET", "status": "error"}),
                serde_json::json!({"method": "POST", "status": "ok"}),
            ],
        )
        .await;

        // Per method: total count + conditional count of status = 'error'.
        let spec = AggregateSpec {
            select_columns: vec!["method".into()],
            aggregates: vec![
                AggregateColumnSpec::Count {
                    alias: "cnt".into(),
                },
                AggregateColumnSpec::CaseWhenSum {
                    when: vec![FilterTree::Leaf(Filter {
                        field: "status".into(),
                        operator: FilterOp::Equal,
                        value: serde_json::json!("error"),
                    })],
                    alias: "errors".into(),
                },
            ],
            filters: vec![],
            group_by: vec![GroupBySpec::Column("method".into())],
            sort: vec![SortField {
                field: "method".into(),
                desc: false,
            }],
            limit: 0,
        };
        let rows = DatabaseService::aggregate(&svc, "reqs", spec)
            .await
            .unwrap();

        assert_eq!(rows.len(), 2);
        // GET first (ascending): 3 total, 2 errors.
        assert_eq!(rows[0].data["method"], serde_json::json!("GET"));
        assert_eq!(rows[0].data["cnt"], serde_json::json!(3));
        assert_eq!(
            rows[0].data["errors"],
            serde_json::json!(2),
            "conditional count of status='error' in GET group"
        );
        // POST: 1 total, 0 errors.
        assert_eq!(rows[1].data["method"], serde_json::json!("POST"));
        assert_eq!(rows[1].data["cnt"], serde_json::json!(1));
        assert_eq!(rows[1].data["errors"], serde_json::json!(0));
    }

    #[tokio::test]
    async fn aggregate_date_bucket_groups_by_day() {
        use wafer_core::interfaces::database::service::{
            AggregateColumnSpec, AggregateSpec, GroupBySpec,
        };

        let svc = make_test_svc();
        // Explicit plain-date `created_at` values so SQLite's date() buckets
        // them deterministically (two on the 15th, one on the 16th).
        seed_rows(
            &svc,
            "events",
            vec![
                serde_json::json!({"kind": "a", "created_at": "2026-01-15"}),
                serde_json::json!({"kind": "b", "created_at": "2026-01-15"}),
                serde_json::json!({"kind": "c", "created_at": "2026-01-16"}),
            ],
        )
        .await;

        let spec = AggregateSpec {
            select_columns: vec![],
            aggregates: vec![AggregateColumnSpec::Count {
                alias: "cnt".into(),
            }],
            filters: vec![],
            group_by: vec![GroupBySpec::DateBucket {
                field: "created_at".into(),
            }],
            sort: vec![SortField {
                field: "created_at".into(),
                desc: false,
            }],
            limit: 0,
        };
        let rows = DatabaseService::aggregate(&svc, "events", spec)
            .await
            .unwrap();

        assert_eq!(rows.len(), 2, "two day buckets");
        assert_eq!(rows[0].data["created_at"], serde_json::json!("2026-01-15"));
        assert_eq!(rows[0].data["cnt"], serde_json::json!(2));
        assert_eq!(rows[1].data["created_at"], serde_json::json!("2026-01-16"));
        assert_eq!(rows[1].data["cnt"], serde_json::json!(1));
    }

    // -----------------------------------------------------------------------
    // Filtered writes: a filter never adds a column. A filter on a column the
    // table lacks is `InvalidArgument` and changes nothing; the SET columns of
    // an update are still added from the data.
    // -----------------------------------------------------------------------

    fn unknown_column_filter(field: &str) -> Vec<Filter> {
        vec![Filter {
            field: field.to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("x"),
        }]
    }

    fn assert_unknown_column(err: &DatabaseError, column: &str) {
        assert!(
            matches!(err, DatabaseError::InvalidArgument(msg) if msg.contains(column)),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn delete_where_on_a_missing_filter_column_is_refused() {
        let svc = make_test_svc();
        seed_rows(&svc, "items", vec![serde_json::json!({"name": "alpha"})]).await;
        let before = DatabaseService::schema_columns(&svc, "items")
            .await
            .unwrap();

        let err =
            DatabaseService::delete_where_count(&svc, "items", &unknown_column_filter("archived"))
                .await
                .expect_err("a filter on a missing column must be refused");
        assert_unknown_column(&err, "archived");
        assert_eq!(
            DatabaseService::schema_columns(&svc, "items")
                .await
                .unwrap(),
            before
        );
        assert_eq!(DatabaseService::count(&svc, "items", &[]).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn update_where_adds_set_columns_but_refuses_a_missing_filter_column() {
        let svc = make_test_svc();
        seed_rows(
            &svc,
            "items",
            vec![
                serde_json::json!({"name": "alpha"}),
                serde_json::json!({"name": "beta"}),
            ],
        )
        .await;
        let before = DatabaseService::schema_columns(&svc, "items")
            .await
            .unwrap();

        // `category` (WHERE) does not exist: refused before `flag` is added.
        let mut patch = std::collections::HashMap::new();
        patch.insert("flag".to_string(), serde_json::json!("on"));
        let err = DatabaseService::update_where(
            &svc,
            "items",
            &unknown_column_filter("category"),
            patch.clone(),
        )
        .await
        .expect_err("a filter on a missing column must be refused");
        assert_unknown_column(&err, "category");
        assert_eq!(
            DatabaseService::schema_columns(&svc, "items")
                .await
                .unwrap(),
            before
        );

        // A filter on a present column: the new SET column `flag` is added.
        let filters = vec![Filter {
            field: "name".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("alpha"),
        }];
        DatabaseService::update_where(&svc, "items", &filters, patch)
            .await
            .expect("a new SET column is added from the data");
        let rows = DatabaseService::list(&svc, "items", &ListOptions::default())
            .await
            .unwrap();
        let flagged = rows
            .records
            .iter()
            .filter(|r| r.data["flag"] == serde_json::json!("on"))
            .count();
        assert_eq!(flagged, 1);
    }

    #[tokio::test]
    async fn take_where_on_a_missing_filter_column_is_refused() {
        let svc = make_test_svc();
        seed_rows(&svc, "codes", vec![serde_json::json!({"code": "abc"})]).await;
        let before = DatabaseService::schema_columns(&svc, "codes")
            .await
            .unwrap();
        let err = DatabaseService::take_where(&svc, "codes", &unknown_column_filter("claimed_by"))
            .await
            .expect_err("a filter on a missing column must be refused");
        assert_unknown_column(&err, "claimed_by");
        assert_eq!(
            DatabaseService::schema_columns(&svc, "codes")
                .await
                .unwrap(),
            before
        );
        assert_eq!(DatabaseService::count(&svc, "codes", &[]).await.unwrap(), 1);
    }

    /// A column an earlier build declared bare `JSON` (NUMERIC affinity) is
    /// still a JSON column: objects and strings written to it read back as
    /// written.
    #[tokio::test]
    async fn a_legacy_json_declared_column_still_reads_as_json() {
        let svc = make_test_svc();
        DatabaseService::exec_raw(
            &svc,
            "CREATE TABLE legacy (id TEXT PRIMARY KEY, meta JSON)",
            &[],
        )
        .await
        .unwrap();
        for (id, meta) in [
            ("obj", serde_json::json!({"a": [1, 2]})),
            ("str", serde_json::json!("123")),
            ("num", serde_json::json!(7)),
        ] {
            let data = [
                ("id".to_string(), serde_json::json!(id)),
                ("meta".to_string(), meta.clone()),
            ]
            .into_iter()
            .collect();
            DatabaseService::create(&svc, "legacy", data).await.unwrap();
            let got = DatabaseService::get(&svc, "legacy", id).await.unwrap();
            assert_eq!(got.data["meta"], meta, "{id}");
        }
    }

    #[tokio::test]
    async fn create_stores_objects_as_json_and_roundtrips() {
        // Objects flow through the shared create default →
        // wafer_sql_utils::query::build_insert → Value::Json → TEXT bind on
        // SQLite, into a column the lazy add declares `JSON`; the read path
        // parses that column's text back into a value.
        let svc = make_test_svc();
        seed_rows(&svc, "items", vec![serde_json::json!({"name": "seed"})]).await;
        let mut data = std::collections::HashMap::new();
        data.insert("name".to_string(), serde_json::json!("with-meta"));
        data.insert("meta".to_string(), serde_json::json!({"a": 1, "b": [true]}));
        let created = DatabaseService::create(&svc, "items", data).await.unwrap();
        assert!(!created.id.is_empty());

        let reread = DatabaseService::get(&svc, "items", &created.id)
            .await
            .unwrap();
        assert_eq!(
            reread.data["meta"],
            serde_json::json!({"a": 1, "b": [true]})
        );
    }

    #[tokio::test]
    async fn create_on_integer_pk_table_returns_generated_rowid() {
        // INTEGER PRIMARY KEY tables generate their own id: create() must not
        // synthesize a UUID, and the rowid from the lock-spanning run_insert
        // is folded into the returned record.
        let svc = make_test_svc();
        exec_batch_for_tests(
            &svc,
            "CREATE TABLE counters (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
        )
        .await;
        let mut data = std::collections::HashMap::new();
        data.insert("name".to_string(), serde_json::json!("first"));
        let created = DatabaseService::create(&svc, "counters", data)
            .await
            .unwrap();
        assert_eq!(created.id, "1");
        assert_eq!(created.data["id"], serde_json::json!(1));

        let reread = DatabaseService::get(&svc, "counters", "1").await.unwrap();
        assert_eq!(reread.data["name"], serde_json::json!("first"));
    }

    #[tokio::test]
    async fn list_skip_count_returns_records_len_as_total_count() {
        let svc = make_test_svc();
        seed_rows(
            &svc,
            "rows",
            vec![
                serde_json::json!({"name": "a"}),
                serde_json::json!({"name": "b"}),
                serde_json::json!({"name": "c"}),
                serde_json::json!({"name": "d"}),
                serde_json::json!({"name": "e"}),
            ],
        )
        .await;

        // With skip_count: true — total_count is records.len(), not full count.
        let opts_skip = ListOptions {
            limit: Some(2),
            skip_count: true,
            ..Default::default()
        };
        let result = DatabaseService::list(&svc, "rows", &opts_skip)
            .await
            .unwrap();
        assert_eq!(result.records.len(), 2);
        assert_eq!(result.total_count, 2);

        // With skip_count: false — total_count is the full collection size.
        let opts_count = ListOptions {
            limit: Some(2),
            skip_count: false,
            ..Default::default()
        };
        let result = DatabaseService::list(&svc, "rows", &opts_count)
            .await
            .unwrap();
        assert_eq!(result.records.len(), 2);
        assert_eq!(result.total_count, 5);
    }

    #[tokio::test]
    async fn list_with_column_projection_returns_only_selected_columns() {
        // `columns: Some([...])` renders `SELECT id, name` (not `SELECT *`),
        // so the unprojected `secret` column must be absent from the returned
        // record even though the row has a value for it.
        let svc = make_test_svc();
        seed_rows(
            &svc,
            "rows",
            vec![serde_json::json!({"name": "a", "secret": "s1"})],
        )
        .await;
        let opts = ListOptions {
            columns: Some(vec!["id".into(), "name".into()]),
            ..Default::default()
        };
        let list = DatabaseService::list(&svc, "rows", &opts).await.unwrap();
        let row = &list.records[0].data;
        assert!(row.contains_key("name"), "projected column present");
        assert!(!row.contains_key("secret"), "unprojected column absent");
        // The projection is honored, but a `None` projection still returns
        // every column — sanity-check the fixture actually stored `secret`.
        let full = DatabaseService::list(&svc, "rows", &ListOptions::default())
            .await
            .unwrap();
        assert_eq!(full.records[0].data["secret"], serde_json::json!("s1"));
    }

    #[tokio::test]
    async fn list_with_any_group_filter_returns_only_or_matching_rows() {
        // A group filter (`Any` = OR) must actually execute against the DB via
        // `filter_tree` → `build_condition_tree` → `extra_condition`. Before
        // Task 4, LIST flattened the tree to empty and returned ALL rows (the
        // Task 3 fail-open). Here `status = 'active' OR status = 'pending'`
        // must return exactly the two matching rows and skip 'archived',
        // and `total_count` (computed with the same extra_condition) must
        // agree with the filtered set — not the full table.
        let svc = make_test_svc();
        seed_rows(
            &svc,
            "rows",
            vec![
                serde_json::json!({"name": "a", "status": "active"}),
                serde_json::json!({"name": "b", "status": "pending"}),
                serde_json::json!({"name": "c", "status": "archived"}),
                serde_json::json!({"name": "d", "status": "archived"}),
            ],
        )
        .await;

        let tree = vec![FilterTree::Any(vec![
            FilterTree::Leaf(Filter {
                field: "status".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!("active"),
            }),
            FilterTree::Leaf(Filter {
                field: "status".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!("pending"),
            }),
        ])];
        let opts = ListOptions {
            filters: Vec::new(),
            filter_tree: Some(tree),
            sort: vec![SortField {
                field: "name".into(),
                desc: false,
            }],
            ..Default::default()
        };
        let list = DatabaseService::list(&svc, "rows", &opts).await.unwrap();

        assert_eq!(
            list.records.len(),
            2,
            "only the two OR-matching rows should return, not all four"
        );
        let statuses: Vec<&str> = list
            .records
            .iter()
            .map(|r| r.data["status"].as_str().unwrap())
            .collect();
        assert_eq!(statuses, vec!["active", "pending"]);
        assert!(
            !statuses.contains(&"archived"),
            "archived rows must be excluded by the group filter"
        );
        assert_eq!(
            list.total_count, 2,
            "total_count must reflect the filtered set (extra_condition applied to COUNT), not the full table"
        );
    }

    #[tokio::test]
    async fn list_with_group_filter_on_column_absent_from_schema_is_refused() {
        // A field that appears ONLY inside a group (`filter_tree`), never in
        // the flat `filters` list, is checked like a flat filter's: a column
        // the table lacks is `InvalidArgument`, and the read adds nothing.
        let svc = make_test_svc();
        seed_rows(&svc, "rows", vec![serde_json::json!({"name": "a"})]).await;
        let before = DatabaseService::schema_columns(&svc, "rows").await.unwrap();

        let tree = vec![FilterTree::Any(vec![FilterTree::Leaf(Filter {
            field: "tier".into(), // absent from the seeded schema
            operator: FilterOp::Equal,
            value: serde_json::json!("gold"),
        })])];
        let opts = ListOptions {
            filter_tree: Some(tree),
            ..Default::default()
        };
        let err = DatabaseService::list(&svc, "rows", &opts)
            .await
            .expect_err("a group-only filter on an unknown column must be refused");
        assert!(
            matches!(&err, DatabaseError::InvalidArgument(msg) if msg.contains("tier")),
            "{err:?}"
        );
        assert_eq!(
            DatabaseService::schema_columns(&svc, "rows").await.unwrap(),
            before,
            "a read must not add a column"
        );
    }

    #[tokio::test]
    async fn get_missing_row_returns_not_found() {
        let svc = make_test_svc();
        seed_rows(&svc, "widgets", vec![serde_json::json!({"name": "a"})]).await;
        let err = DatabaseService::get(&svc, "widgets", "no-such-id")
            .await
            .unwrap_err();
        assert!(matches!(err, DatabaseError::NotFound));
    }

    #[tokio::test]
    async fn count_on_missing_table_returns_zero() {
        let svc = make_test_svc();
        let n = DatabaseService::count(&svc, "no_such_table", &[])
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn update_lazily_adds_a_column_absent_from_the_schema() {
        // Regression for the M18 fix: SQLite `update` ensures columns from
        // the update payload (matching Postgres) — now via the shared
        // `DbExec::ensure_data_columns` default. Previously updating a key
        // absent from the table failed with a confusing "no such column";
        // now the column is added.
        let svc = make_test_svc();
        seed_rows(&svc, "widgets", vec![serde_json::json!({"name": "a"})]).await;
        let created = DatabaseService::list(&svc, "widgets", &ListOptions::default())
            .await
            .unwrap();
        let id = created.records[0].id.clone();

        let mut patch = std::collections::HashMap::new();
        // `nickname` is not in the seeded schema.
        patch.insert("nickname".to_string(), serde_json::json!("ace"));
        let updated = DatabaseService::update(&svc, "widgets", &id, patch)
            .await
            .expect("update should add the missing column and succeed");
        assert_eq!(updated.data["nickname"], serde_json::json!("ace"));

        // The column now exists and round-trips on a fresh read.
        let reread = DatabaseService::get(&svc, "widgets", &id).await.unwrap();
        assert_eq!(reread.data["nickname"], serde_json::json!("ace"));
    }

    #[tokio::test]
    async fn schema_table_exists_reflects_creation() {
        let svc = make_test_svc();
        assert!(!DatabaseService::schema_table_exists(&svc, "widgets")
            .await
            .unwrap());
        seed_rows(&svc, "widgets", vec![serde_json::json!({"name": "a"})]).await;
        assert!(DatabaseService::schema_table_exists(&svc, "widgets")
            .await
            .unwrap());
    }

    // -----------------------------------------------------------------------
    // PERF-02 worker routing (file-backed SQLite with read pool)
    // -----------------------------------------------------------------------

    /// Unique on-disk DB path; minimal tempfile stand-in (no new dev-dep).
    fn tempdb_path(tag: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        std::env::temp_dir().join(format!(
            "wafer-sqlite-test-{tag}-{}-{nonce}.db",
            std::process::id()
        ))
    }

    /// PERF-02 evidence: reads are served by the read-only reader workers,
    /// so they complete while the write worker is stalled. Deterministic:
    /// the stall job is enqueued on the write worker FIRST and holds it
    /// until the gate releases — were reads routed through the write
    /// worker they would queue behind the stall and the timeout would hit.
    #[tokio::test(flavor = "multi_thread")]
    async fn reads_proceed_while_write_worker_is_stalled() {
        let path = tempdb_path("stall");
        let svc = SQLiteDatabaseService::open(path.to_str().unwrap()).unwrap();
        assert!(
            !svc.readers.is_empty(),
            "file-backed service must open reader workers"
        );
        seed_rows(&svc, "rows", vec![serde_json::json!({"id": "a", "v": "1"})]).await;

        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        let stalled = {
            let write = svc.write.clone();
            tokio::spawn(async move {
                write
                    .run(move |_conn| {
                        let _ = gate_rx.recv();
                    })
                    .await
            })
        };

        let got = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            DatabaseService::get(&svc, "rows", "a"),
        )
        .await
        .expect("read must not queue behind the stalled write worker")
        .expect("get");
        assert_eq!(got.id, "a");

        gate_tx.send(()).expect("stall job dropped its gate");
        stalled
            .await
            .expect("join")
            .expect("stall job must complete");
        let _ = std::fs::remove_file(&path);
    }

    /// The fetch/scalar paths run on read-only connections: a write
    /// statement smuggled through them errors and mutates nothing (the
    /// mutex-era code silently EXECUTED such writes).
    ///
    /// Covers `run_fetch_one`, `run_scalar_i64` (both `query_row`, which
    /// already propagated) *and* `run_fetch` (`query_map` +
    /// [`fetch_rows`](SQLiteDatabaseService::fetch_rows)'s fallible
    /// `collect`) — before that fallible collect existed, a write smuggled
    /// through `run_fetch` returned `Ok(vec![])` instead of erroring, the
    /// exact swallow this test exists to rule out on every fetch path, not
    /// just the two that happened to use `query_row`.
    #[tokio::test]
    async fn fetch_path_is_read_only_on_file_backed_service() {
        let path = tempdb_path("ro");
        let svc = SQLiteDatabaseService::open(path.to_str().unwrap()).unwrap();
        assert!(
            !svc.readers.is_empty(),
            "file-backed service must open reader workers"
        );
        seed_rows(&svc, "rows", vec![serde_json::json!({"id": "a", "v": "1"})]).await;

        let err = svc
            .run_fetch_one(
                "INSERT INTO rows (id) VALUES ('evil')",
                &[],
                JsonColumns::NONE,
            )
            .await
            .expect_err("write through the read path must fail");
        assert!(
            err.to_string().to_lowercase().contains("readonly"),
            "expected a readonly-database error, got: {err}"
        );

        let err = svc
            .run_fetch(
                "INSERT INTO rows (id) VALUES ('evil2') RETURNING *",
                &[],
                JsonColumns::NONE,
            )
            .await
            .expect_err("write through run_fetch must error, not return Ok([])");
        assert!(
            err.to_string().to_lowercase().contains("readonly"),
            "expected a readonly-database error, got: {err}"
        );

        let count = svc
            .run_scalar_i64("SELECT COUNT(*) FROM rows", &[])
            .await
            .expect("count");
        assert_eq!(
            count, 1,
            "neither smuggled write (INSERT, INSERT … RETURNING) must have executed"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// `take_where` builds `DELETE … RETURNING` and must consume the rows it
    /// returns, even on a file-backed service with dedicated read-only reader
    /// connections. Before the fix, `take_where` ran the `DELETE … RETURNING`
    /// through `run_fetch` — the read path. A reader connection is opened
    /// `SQLITE_OPEN_READ_ONLY`, so the write inside `RETURNING` fails at the
    /// first `step()` with a "readonly database" error — but `run_fetch`
    /// decodes rows via `query_map(...).filter_map(...)`, which treats *any*
    /// per-row `Err` (including this one) as "row failed to decode, skip it
    /// and warn" rather than "the statement failed." So the error is
    /// swallowed, `take_where` returns an empty `Vec` with no error at all,
    /// and the rows are neither returned nor deleted — silent data loss with
    /// no signal to the caller. The single-connection in-memory tests
    /// (`take_where_returns_deleted_rows` etc.) can't catch this: they share
    /// one connection, so there is no read/write split to get wrong.
    #[tokio::test]
    async fn take_where_consumes_rows_on_file_backed_service() {
        let path = tempdb_path("take-where");
        let svc = SQLiteDatabaseService::open(path.to_str().unwrap()).unwrap();
        assert!(
            !svc.readers.is_empty(),
            "file-backed service must open reader workers"
        );
        seed_rows(
            &svc,
            "codes",
            vec![
                serde_json::json!({"code": "abc123", "used": false}),
                serde_json::json!({"code": "xyz789", "used": false}),
                serde_json::json!({"code": "def456", "used": true}),
            ],
        )
        .await;

        let filters = vec![Filter {
            field: "used".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(false),
        }];

        let taken = DatabaseService::take_where(&svc, "codes", &filters)
            .await
            .expect("take_where");
        let mut taken_codes: Vec<String> = taken
            .iter()
            .map(|r| r.data["code"].as_str().unwrap().to_string())
            .collect();
        taken_codes.sort();
        assert_eq!(
            taken_codes,
            vec!["abc123".to_string(), "xyz789".to_string()],
            "take_where must return the matching rows"
        );

        // The rows must actually be gone: a following list/get must not find
        // them. This is the assertion that fails on the pre-fix code — the
        // rows come back from `take_where` but the DELETE never applied on a
        // file-backed service, so they are still there afterward.
        let remaining = DatabaseService::list(&svc, "codes", &ListOptions::default())
            .await
            .expect("list");
        assert_eq!(
            remaining.total_count, 1,
            "the two taken rows must be deleted, leaving only the used one"
        );
        let remaining_codes: Vec<String> = remaining
            .records
            .iter()
            .map(|r| r.data["code"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(remaining_codes, vec!["def456".to_string()]);

        for code in ["abc123", "xyz789"] {
            let found = DatabaseService::list(
                &svc,
                "codes",
                &ListOptions {
                    filters: vec![Filter {
                        field: "code".to_string(),
                        operator: FilterOp::Equal,
                        value: serde_json::json!(code),
                    }],
                    ..ListOptions::default()
                },
            )
            .await
            .expect("list by code");
            assert!(
                found.records.is_empty(),
                "taken row {code} must no longer be gettable/listable"
            );
        }

        let _ = std::fs::remove_file(&path);
    }

    // -----------------------------------------------------------------------
    // STRICT_SCHEMA mode + schema-cache invalidation
    // -----------------------------------------------------------------------

    /// STRICT_SCHEMA skips the per-op table-exists guard: a query against a
    /// missing table errors (the SELECT runs) instead of returning empty.
    #[tokio::test]
    async fn strict_schema_skips_table_exists_guard() {
        let svc = make_test_svc();

        // Non-strict: the exists guard fires, so a missing table lists empty.
        let empty = DatabaseService::list(&svc, "ghost", &ListOptions::default())
            .await
            .expect("non-strict list on a missing table is empty, not an error");
        assert!(empty.records.is_empty());

        // Strict: the guard is skipped, so the SELECT hits the missing table
        // and errors — proving the (now-cached exists=false) probe was bypassed.
        svc.set_strict_schema(true);
        let err = DatabaseService::list(&svc, "ghost", &ListOptions::default())
            .await
            .expect_err("strict mode skips the exists guard; a missing table must error");
        assert!(
            err.to_string().to_lowercase().contains("no such table"),
            "expected a missing-table error, got: {err}"
        );
    }

    /// STRICT_SCHEMA skips the lazy column-add ALTER path: a write referencing
    /// an unmigrated column fails loudly instead of the column being
    /// synthesized. The same write succeeds (column auto-added) when strict.
    #[tokio::test]
    async fn strict_schema_skips_lazy_column_add() {
        let svc = make_test_svc();
        let table = Table {
            name: "t".into(),
            columns: vec![
                pk("id"),
                Column::new("created_at", DataType::Text).null(),
                Column::new("updated_at", DataType::Text).null(),
            ],
            indexes: Vec::new(),
            primary_key: Vec::new(),
            unique_keys: Vec::new(),
        };
        DatabaseService::ensure_schema_table(&svc, &table)
            .await
            .unwrap();

        svc.set_strict_schema(true);
        let mut row = std::collections::HashMap::new();
        row.insert("id".to_string(), serde_json::json!("r1"));
        row.insert("extra".to_string(), serde_json::json!("v"));
        let err = DatabaseService::create(&svc, "t", row)
            .await
            .expect_err("strict mode must not synthesize the missing `extra` column");
        assert!(
            err.to_string().to_lowercase().contains("no column"),
            "expected a missing-column error, got: {err}"
        );

        // Non-strict lazily adds the column and the identical write succeeds.
        svc.set_strict_schema(false);
        let mut row2 = std::collections::HashMap::new();
        row2.insert("id".to_string(), serde_json::json!("r2"));
        row2.insert("extra".to_string(), serde_json::json!("v"));
        DatabaseService::create(&svc, "t", row2)
            .await
            .expect("non-strict lazily adds the column");
    }

    /// Dropping a table must invalidate its cached exists=true fact, so a
    /// follow-up query re-probes and returns empty rather than trusting the
    /// stale entry and erroring on a SELECT against the vanished table.
    #[tokio::test]
    async fn schema_cache_invalidated_on_drop_table() {
        let svc = make_test_svc();
        seed_rows(&svc, "temp", vec![serde_json::json!({"v": "1"})]).await;

        // Warm the exists cache (and columns cache) via a real list.
        let listed = DatabaseService::list(&svc, "temp", &ListOptions::default())
            .await
            .unwrap();
        assert_eq!(listed.records.len(), 1);

        DatabaseService::schema_drop_table(&svc, "temp")
            .await
            .unwrap();

        let after = DatabaseService::list(&svc, "temp", &ListOptions::default())
            .await
            .expect("a stale exists cache would error here; invalidation returns empty");
        assert!(after.records.is_empty());
        assert_eq!(after.total_count, 0);
    }

    /// A request naming an unknown column is refused without evicting the
    /// cached column list the other requests are served from: the uncached
    /// re-read that confirms the column is missing leaves the cache alone.
    #[tokio::test]
    async fn unknown_column_requests_do_not_evict_the_schema_cache() {
        let svc = make_test_svc();
        seed_rows(&svc, "widgets", vec![serde_json::json!({"name": "a"})]).await;
        let named_a = vec![Filter {
            field: "name".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("a"),
        }];
        let _ = DatabaseService::count(&svc, "widgets", &named_a)
            .await
            .unwrap();
        let cache = DbExec::schema_cache(&svc).expect("the SQLite backend caches");
        let generation = cache.generation();
        assert!(
            cache.columns("widgets").is_some(),
            "the column list is cached"
        );

        let unknown = vec![Filter {
            field: "zz".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("x"),
        }];
        for _ in 0..3 {
            let err = DatabaseService::count(&svc, "widgets", &unknown)
                .await
                .expect_err("an unknown column is refused");
            assert!(matches!(err, DatabaseError::InvalidArgument(_)), "{err:?}");
        }
        assert!(cache.columns("widgets").is_some(), "the entry survives");
        assert_eq!(cache.generation(), generation, "nothing was invalidated");
    }

    /// Adding a column out-of-band (via `schema_add_column`) invalidates the
    /// cached column list, so a subsequent filtered query sees the new column
    /// instead of a stale set that omits it.
    #[tokio::test]
    async fn schema_cache_invalidated_on_add_column() {
        let svc = make_test_svc();
        seed_rows(&svc, "widgets", vec![serde_json::json!({"name": "a"})]).await;

        // Warm the column cache: a filter makes `count` read the column list.
        let named_a = vec![Filter {
            field: "name".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("a"),
        }];
        let _ = DatabaseService::count(&svc, "widgets", &named_a)
            .await
            .unwrap();
        let cache = DbExec::schema_cache(&svc).expect("the SQLite backend caches");
        assert!(
            cache.columns("widgets").is_some(),
            "the column list is cached"
        );

        // Add a real column out of band; the cached column list must be
        // dropped. Asserted on the cache itself: the column check re-reads a
        // list that lacks a name before refusing, so a query alone would not
        // tell a dropped entry from a stale one.
        DatabaseService::schema_add_column(
            &svc,
            "widgets",
            &Column::new("flag", DataType::Text).null(),
        )
        .await
        .unwrap();
        assert!(
            cache.columns("widgets").is_none(),
            "add_column drops the entry"
        );

        // A filter on the freshly-added column resolves against the true
        // schema.
        let filters = vec![Filter {
            field: "flag".to_string(),
            operator: FilterOp::IsNull,
            value: serde_json::Value::Null,
        }];
        let n = DatabaseService::count(&svc, "widgets", &filters)
            .await
            .expect("cache must reflect the added column after invalidation");
        assert_eq!(n, 1, "the one seeded row has flag IS NULL");
    }
}
