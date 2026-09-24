//! SQLite-backed VectorService using sqlite-vec for similarity search
//! and FTS5 for optional keyword search.

use rusqlite::{
    functions::{Context, FunctionFlags},
    params, Connection, OptionalExtension, TransactionBehavior,
};
use wafer_core::interfaces::vector::{
    rrf,
    service::{
        check_rename, ColumnInfo, DescribeIndexResponse, DistanceMetric, MetadataFilter,
        SearchMode, VectorEntry, VectorError, VectorIndexConfig, VectorMatch, VectorService,
    },
};
use wafer_sql_utils::vector::{
    build_list_meta_tables, vec0_module_args, VectorIndexRename, VectorIndexSchema,
    METADATA_FILTER_FN,
};

use crate::{
    ensure_vec_loaded,
    worker::{ConnWorker, WORKER_GONE},
};

/// `VectorService` backed by SQLite + `sqlite-vec` (`vec0` virtual
/// tables) for ANN search and FTS5 for keyword search. A dedicated worker
/// thread owns the `rusqlite::Connection` (see [`ConnWorker`]) so vector
/// I/O never blocks an async executor thread (PERF-02); callers are
/// expected to register the `sqlite-vec` auto-extension before opening
/// the connection (see [`crate::ensure_vec_loaded`]).
pub struct SqliteVecService {
    worker: ConnWorker,
}

impl SqliteVecService {
    /// Wrap an existing `rusqlite::Connection` that already has the
    /// `sqlite-vec` extension loaded. Used by the consuming application to bind a
    /// shared on-disk DB to the vector service.
    ///
    /// When the file is shared with other writers (such as the database
    /// service), `db` must carry a busy timeout: `upsert` and `delete` wait
    /// for another writer's lock through the busy handler, and without one
    /// they fail with `SQLITE_BUSY` at once. `Connection::open` sets a 5 s
    /// timeout; a caller that changes it chooses how long vector writes wait.
    ///
    /// Registers [`METADATA_FILTER_FN`] on `db`, which filtered searches call.
    pub fn new(db: Connection) -> rusqlite::Result<Self> {
        db.create_scalar_function(
            METADATA_FILTER_FN,
            2,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            metadata_filter_fn,
        )?;
        Ok(Self {
            worker: ConnWorker::spawn(db, "sqlite-vec"),
        })
    }

    /// Open an in-memory SQLite connection with `sqlite-vec` registered
    /// via [`crate::ensure_vec_loaded`]. Intended for tests — registration
    /// happens on a throwaway probe connection first because
    /// `sqlite3_auto_extension` only affects connections opened after it.
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        // Register the sqlite-vec auto-extension BEFORE opening the connection.
        // `sqlite3_auto_extension` only affects connections opened after
        // registration, so a conn opened first will not have vec0 available.
        let probe = Connection::open_in_memory()?;
        ensure_vec_loaded(&probe)?;
        drop(probe);
        Self::new(Connection::open_in_memory()?)
    }

    /// Run a job on the connection worker, mapping a dead worker to
    /// [`VectorError::Internal`]. Whole methods run as ONE job, preserving
    /// the previous continuous-lock semantics (transactions included).
    async fn on_conn<T, F>(&self, f: F) -> Result<T, VectorError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, VectorError> + Send + 'static,
    {
        self.worker
            .run(f)
            .await
            .map_err(|()| VectorError::Internal(WORKER_GONE.to_string()))?
    }

    /// Validate `name` and compute the index's table names. Non-identifier
    /// names are rejected fail-closed — they would otherwise be spliced into
    /// SQL identifier positions.
    fn schema_for(name: &str) -> Result<VectorIndexSchema, VectorError> {
        VectorIndexSchema::new(name).map_err(|_| VectorError::InvalidIndexName(name.to_string()))
    }

    fn table_exists(conn: &Connection, table: &str) -> Result<bool, VectorError> {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                params![table],
                |row| row.get(0),
            )
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        Ok(exists)
    }

    /// Whether any catalog entry is named `name` as SQLite resolves names —
    /// ignoring ASCII case — other than the table named exactly `except`.
    /// This is the test for "creating or renaming a table to `name` would
    /// collide", with the table being moved (`except`) left out.
    fn name_taken(conn: &Connection, name: &str, except: &str) -> Result<bool, VectorError> {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master \
             WHERE name = ?1 COLLATE NOCASE AND name <> ?2)",
            params![name, except],
            |row| row.get(0),
        )
        .map_err(|e| VectorError::Internal(e.to_string()))
    }

    /// Worker-side body of [`VectorService::rename_index`]: catalog probes,
    /// then every move in one IMMEDIATE transaction.
    fn rename_on_conn(
        conn: &mut Connection,
        rename: &VectorIndexRename,
        from: &str,
        to: &str,
    ) -> Result<(), VectorError> {
        ensure_vec_loaded(conn).map_err(|e| VectorError::Internal(e.to_string()))?;
        // IMMEDIATE for the same reason as in `upsert_on_conn`; it also holds
        // the catalog still between the probes below and the moves.
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| VectorError::Internal(e.to_string()))?;

        // `from` is matched exactly: an index stored under another spelling
        // of the name is not this one.
        let vec_sql: Option<String> = tx
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
                params![&rename.from.vec_table],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        let Some(vec_sql) = vec_sql else {
            return Err(VectorError::IndexNotFound(from.to_string()));
        };
        if !Self::table_exists(&tx, &rename.from.meta_table)? {
            return Err(VectorError::Internal(format!(
                "vector index {from:?} has no table {:?}",
                rename.from.meta_table
            )));
        }
        let keyword_search = Self::table_exists(&tx, &rename.from.fts_table)?;

        // None of `to`'s tables may exist under any spelling, bar the table
        // of `from` that becomes it. That includes an FTS table when `from`
        // has none: the moved index would pick it up as its keyword search.
        for (target, own) in [
            (&rename.to.vec_table, &rename.from.vec_table),
            (&rename.to.meta_table, &rename.from.meta_table),
            (&rename.to.fts_table, &rename.from.fts_table),
        ] {
            if Self::name_taken(&tx, target, own)? {
                return Err(VectorError::IndexAlreadyExists(to.to_string()));
            }
        }

        let module_args = vec0_module_args(&vec_sql).ok_or_else(|| {
            VectorError::Internal(format!(
                "vector table {:?} is not a virtual table: {vec_sql}",
                rename.from.vec_table
            ))
        })?;
        let mut stmt = tx
            .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        let vec_columns = stmt
            .query_map(params![&rename.from.vec_table], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| VectorError::Internal(e.to_string()))?
            .collect::<Result<Vec<String>, _>>()
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        drop(stmt);

        for stmt in rename.build_statements(module_args, &vec_columns, keyword_search) {
            tx.execute(&stmt.sql, [])
                .map_err(|e| VectorError::Internal(e.to_string()))?;
        }
        tx.commit()
            .map_err(|e| VectorError::Internal(e.to_string()))
    }

    fn index_exists(conn: &Connection, schema: &VectorIndexSchema) -> Result<bool, VectorError> {
        Self::table_exists(conn, &schema.vec_table)
    }

    fn has_keyword_search(
        conn: &Connection,
        schema: &VectorIndexSchema,
    ) -> Result<bool, VectorError> {
        Self::table_exists(conn, &schema.fts_table)
    }

    /// Worker-side body of [`VectorService::upsert`]: validation that needs
    /// the connection, then one transaction spanning every entry.
    fn upsert_on_conn(
        conn: &mut Connection,
        schema: &VectorIndexSchema,
        index: &str,
        entries: Vec<VectorEntry>,
    ) -> Result<(), VectorError> {
        ensure_vec_loaded(conn).map_err(|e| VectorError::Internal(e.to_string()))?;
        if !Self::index_exists(conn, schema)? {
            return Err(VectorError::IndexNotFound(index.to_string()));
        }
        let has_kw = Self::has_keyword_search(conn, schema)?;
        for e in &entries {
            if has_kw && e.text.is_none() {
                return Err(VectorError::TextRequired);
            }
        }

        let select_rowid_sql = schema.build_select_rowid_by_id().sql;
        let delete_vec_sql = schema.build_delete_vec_by_rowid().sql;
        let insert_meta_sql = schema.build_insert_meta_autoinc().sql;
        let insert_vec_sql = schema.build_insert_vec().sql;
        let update_meta_sql = schema.build_update_meta().sql;
        let delete_fts_sql = schema.build_delete_fts_by_id().sql;
        let insert_fts_sql = schema.build_insert_fts().sql;

        // IMMEDIATE takes the write lock up front, waiting through the busy
        // handler (the connection's busy timeout, see `new`) if another
        // connection to the same file is writing. A
        // DEFERRED transaction would read first and then fail to upgrade to
        // a writer with SQLITE_BUSY, without waiting, whenever another
        // connection holds or has just committed a write.
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| VectorError::Internal(e.to_string()))?;

        for e in entries {
            let meta_json = e
                .metadata
                .as_ref()
                .map_or_else(|| "{}".into(), |v| v.to_string());

            // Find existing rowid (re-upsert path) or create a new one.
            let rowid: Option<i64> = tx
                .query_row(&select_rowid_sql, params![&e.id], |r| r.get(0))
                .optional()
                .map_err(|err| VectorError::Internal(err.to_string()))?;
            let rowid = match rowid {
                Some(rid) => {
                    tx.execute(&delete_vec_sql, params![rid])
                        .map_err(|err| VectorError::Internal(err.to_string()))?;
                    rid
                }
                None => {
                    tx.execute(
                        &insert_meta_sql,
                        params![&e.id, meta_json, e.text.clone().unwrap_or_default()],
                    )
                    .map_err(|err| VectorError::Internal(err.to_string()))?;
                    tx.query_row(&select_rowid_sql, params![&e.id], |r| r.get::<_, i64>(0))
                        .map_err(|err| VectorError::Internal(err.to_string()))?
                }
            };

            let vec_bytes: Vec<u8> = e.vector.iter().flat_map(|f| f.to_le_bytes()).collect();
            tx.execute(&insert_vec_sql, params![rowid, vec_bytes])
                .map_err(|err| VectorError::Internal(err.to_string()))?;

            // Update meta (metadata + text may have changed on re-upsert)
            tx.execute(
                &update_meta_sql,
                params![meta_json, e.text.clone().unwrap_or_default(), &e.id],
            )
            .map_err(|err| VectorError::Internal(err.to_string()))?;

            if has_kw {
                let text = e.text.unwrap_or_default();
                tx.execute(&delete_fts_sql, params![&e.id])
                    .map_err(|err| VectorError::Internal(err.to_string()))?;
                tx.execute(&insert_fts_sql, params![&e.id, text])
                    .map_err(|err| VectorError::Internal(err.to_string()))?;
            }
        }

        tx.commit()
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl VectorService for SqliteVecService {
    async fn create_index(&self, config: VectorIndexConfig) -> Result<(), VectorError> {
        let schema = Self::schema_for(&config.name)?;
        // All 3 metric variants are accepted — sqlite-vec is distance-agnostic at storage;
        // it operates as cosine distance in SQL queries regardless of the stored metric tag.
        let _ = match config.metric {
            DistanceMetric::Cosine | DistanceMetric::Euclidean | DistanceMetric::DotProduct => (),
        };
        self.on_conn(move |conn| {
            ensure_vec_loaded(conn).map_err(|e| VectorError::Internal(e.to_string()))?;
            if Self::index_exists(conn, &schema)? {
                return Err(VectorError::IndexAlreadyExists(config.name));
            }
            conn.execute_batch(&schema.build_create_vec_and_meta(config.dimensions).sql)
                .map_err(|e| VectorError::Internal(e.to_string()))?;
            if config.keyword_search {
                conn.execute_batch(&schema.build_create_fts().sql)
                    .map_err(|e| VectorError::Internal(e.to_string()))?;
            }
            Ok(())
        })
        .await
    }

    async fn delete_index(&self, name: &str) -> Result<(), VectorError> {
        let schema = Self::schema_for(name)?;
        let name = name.to_string();
        self.on_conn(move |conn| {
            ensure_vec_loaded(conn).map_err(|e| VectorError::Internal(e.to_string()))?;
            if !Self::index_exists(conn, &schema)? {
                return Err(VectorError::IndexNotFound(name));
            }
            for drop_stmt in schema.build_drop_all() {
                conn.execute(&drop_stmt.sql, [])
                    .map_err(|e| VectorError::Internal(e.to_string()))?;
            }
            Ok(())
        })
        .await
    }

    async fn upsert(&self, index: &str, entries: Vec<VectorEntry>) -> Result<(), VectorError> {
        if entries.is_empty() {
            return Ok(());
        }
        let schema = Self::schema_for(index)?;
        let index = index.to_string();
        self.on_conn(move |conn| Self::upsert_on_conn(conn, &schema, &index, entries))
            .await
    }

    async fn query(
        &self,
        index: &str,
        vector: Vec<f32>,
        top_k: usize,
        filter: Option<MetadataFilter>,
        mode: SearchMode,
        keyword_query: Option<String>,
    ) -> Result<Vec<VectorMatch>, VectorError> {
        let schema = Self::schema_for(index)?;
        let index = index.to_string();
        self.on_conn(move |conn| {
            Self::query_on_conn(
                conn,
                &schema,
                &index,
                &vector,
                top_k,
                filter.as_ref(),
                mode,
                keyword_query.as_deref(),
            )
        })
        .await
    }

    async fn delete(&self, index: &str, ids: Vec<String>) -> Result<(), VectorError> {
        if ids.is_empty() {
            return Ok(());
        }
        let schema = Self::schema_for(index)?;
        let index = index.to_string();
        self.on_conn(move |conn| {
            ensure_vec_loaded(conn).map_err(|e| VectorError::Internal(e.to_string()))?;
            if !Self::index_exists(conn, &schema)? {
                return Err(VectorError::IndexNotFound(index));
            }
            let has_kw = Self::has_keyword_search(conn, &schema)?;

            // IMMEDIATE for the same reason as in `upsert_on_conn`.
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| VectorError::Internal(e.to_string()))?;

            // Gather rowids first so we can delete from _vec by rowid.
            let mut stmt = tx
                .prepare(&schema.build_select_rowid_in(ids.len()).sql)
                .map_err(|e| VectorError::Internal(e.to_string()))?;
            let rowids: Vec<i64> = stmt
                .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                    r.get::<_, i64>(0)
                })
                .map_err(|e| VectorError::Internal(e.to_string()))?
                .collect::<rusqlite::Result<Vec<i64>>>()
                .map_err(|e| VectorError::Internal(e.to_string()))?;
            drop(stmt);

            let delete_vec_sql = schema.build_delete_vec_by_rowid().sql;
            for rid in rowids {
                tx.execute(&delete_vec_sql, params![rid])
                    .map_err(|e| VectorError::Internal(e.to_string()))?;
            }
            tx.execute(
                &schema.build_delete_meta_in(ids.len()).sql,
                rusqlite::params_from_iter(ids.iter()),
            )
            .map_err(|e| VectorError::Internal(e.to_string()))?;
            if has_kw {
                tx.execute(
                    &schema.build_delete_fts_in(ids.len()).sql,
                    rusqlite::params_from_iter(ids.iter()),
                )
                .map_err(|e| VectorError::Internal(e.to_string()))?;
            }
            tx.commit()
                .map_err(|e| VectorError::Internal(e.to_string()))?;
            Ok(())
        })
        .await
    }

    async fn count(&self, index: &str) -> Result<u64, VectorError> {
        let schema = Self::schema_for(index)?;
        let index = index.to_string();
        self.on_conn(move |conn| {
            ensure_vec_loaded(conn).map_err(|e| VectorError::Internal(e.to_string()))?;
            if !Self::index_exists(conn, &schema)? {
                return Err(VectorError::IndexNotFound(index));
            }
            let n: i64 = conn
                .query_row(&schema.build_count_meta().sql, [], |r| r.get(0))
                .map_err(|e| VectorError::Internal(e.to_string()))?;
            Ok(n as u64)
        })
        .await
    }

    async fn rename_index(&self, from: &str, to: &str) -> Result<(), VectorError> {
        check_rename(from, to)?;
        let rename = VectorIndexRename::new(from, to).map_err(|_| VectorError::InvalidRename {
            from: from.to_string(),
            to: to.to_string(),
        })?;
        let (from, to) = (from.to_string(), to.to_string());
        self.on_conn(move |conn| Self::rename_on_conn(conn, &rename, &from, &to))
            .await
    }

    async fn list_indexes(&self, prefix: &str) -> Result<Vec<String>, VectorError> {
        let (sql, pattern) = build_list_meta_tables(prefix);
        self.on_conn(move |conn| {
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| VectorError::Internal(e.to_string()))?;
            let names = stmt
                .query_map(params![pattern], |row| row.get::<_, String>(0))
                .map_err(|e| VectorError::Internal(e.to_string()))?
                .collect::<Result<Vec<String>, _>>()
                .map_err(|e| VectorError::Internal(e.to_string()))?;
            // The LIKE pattern guarantees the `_meta` suffix; strip it to stems.
            Ok(names
                .into_iter()
                .filter_map(|n| n.strip_suffix("_meta").map(str::to_string))
                .collect())
        })
        .await
    }

    async fn describe_index(&self, index: &str) -> Result<DescribeIndexResponse, VectorError> {
        let schema = Self::schema_for(index)?;
        self.on_conn(move |conn| {
            // Keyed on the meta table (same source the catalog scan uses), not
            // the vec table — describe reports the meta table's real state.
            if !Self::table_exists(conn, &schema.meta_table)? {
                return Ok(DescribeIndexResponse {
                    exists: false,
                    columns: Vec::new(),
                    keyword_search: false,
                });
            }
            let mut stmt = conn
                .prepare("SELECT name, type FROM pragma_table_info(?1) ORDER BY cid")
                .map_err(|e| VectorError::Internal(e.to_string()))?;
            let columns = stmt
                .query_map(params![&schema.meta_table], |row| {
                    Ok(ColumnInfo {
                        name: row.get(0)?,
                        sql_type: row.get(1)?,
                    })
                })
                .map_err(|e| VectorError::Internal(e.to_string()))?
                .collect::<Result<Vec<ColumnInfo>, _>>()
                .map_err(|e| VectorError::Internal(e.to_string()))?;
            drop(stmt);
            let keyword_search = Self::has_keyword_search(conn, &schema)?;
            Ok(DescribeIndexResponse {
                exists: true,
                columns,
                keyword_search,
            })
        })
        .await
    }

    async fn list_ids(
        &self,
        index: &str,
        filter: MetadataFilter,
    ) -> Result<Vec<String>, VectorError> {
        if filter.equals.is_empty() {
            return Err(VectorError::InvalidMetadataFilter(
                "filter.equals must contain at least one condition".into(),
            ));
        }
        let mut binds: Vec<rusqlite::types::Value> = Vec::with_capacity(filter.equals.len() * 2);
        for (path, value) in &filter.equals {
            binds.push(rusqlite::types::Value::Text(format!("$.{path}")));
            match value {
                serde_json::Value::String(s) => {
                    binds.push(rusqlite::types::Value::Text(s.clone()));
                }
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        binds.push(rusqlite::types::Value::Integer(i));
                    } else if let Some(f) = n.as_f64() {
                        binds.push(rusqlite::types::Value::Real(f));
                    } else {
                        return Err(VectorError::InvalidMetadataFilter(format!(
                            "unrepresentable number for path {path:?}"
                        )));
                    }
                }
                other => {
                    return Err(VectorError::InvalidMetadataFilter(format!(
                        "value for path {path:?} must be a JSON string or number, got {other}"
                    )));
                }
            }
        }
        let schema = Self::schema_for(index)?;
        let index = index.to_string();
        let sql = schema.build_select_ids_by_metadata(filter.equals.len()).sql;
        self.on_conn(move |conn| {
            if !Self::table_exists(conn, &schema.meta_table)? {
                return Err(VectorError::IndexNotFound(index));
            }
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| VectorError::Internal(e.to_string()))?;
            let ids = stmt
                .query_map(rusqlite::params_from_iter(binds.iter()), |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|e| VectorError::Internal(e.to_string()))?
                .collect::<Result<Vec<String>, _>>()
                .map_err(|e| VectorError::Internal(e.to_string()))?;
            Ok(ids)
        })
        .await
    }
}

impl SqliteVecService {
    /// Worker-side body of [`VectorService::query`]: filtered candidate
    /// ranking, fusion and metadata lookup, all on the worker thread.
    #[expect(
        clippy::too_many_arguments,
        reason = "1:1 with the trait method's parameters plus the connection and parsed schema"
    )]
    fn query_on_conn(
        conn: &mut Connection,
        schema: &VectorIndexSchema,
        index: &str,
        vector: &[f32],
        top_k: usize,
        filter: Option<&MetadataFilter>,
        mode: SearchMode,
        keyword_query: Option<&str>,
    ) -> Result<Vec<VectorMatch>, VectorError> {
        ensure_vec_loaded(conn).map_err(|e| VectorError::Internal(e.to_string()))?;
        if !Self::index_exists(conn, schema)? {
            return Err(VectorError::IndexNotFound(index.to_string()));
        }
        let has_kw = Self::has_keyword_search(conn, schema)?;
        match mode {
            SearchMode::Keyword | SearchMode::Hybrid if !has_kw => {
                return Err(VectorError::KeywordSearchNotEnabled);
            }
            SearchMode::Keyword | SearchMode::Hybrid if keyword_query.is_none() => {
                return Err(VectorError::KeywordQueryRequired(mode));
            }
            _ => {}
        }

        let candidate_limit = match mode {
            SearchMode::Vector => top_k,
            _ => top_k.max(50),
        };

        // A non-empty filter restricts the candidates inside each ranking
        // query, before its LIMIT, so every ranking holds only matching
        // entries and a query returns the top `top_k` of the filtered set.
        // Filtering after the LIMIT would drop matches that rank below
        // non-matching entries and return fewer than `top_k`, or none.
        let filter_json = filter
            .filter(|f| !f.equals.is_empty())
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| VectorError::Internal(e.to_string()))?;

        // --- Vector rankings ---
        let vec_ranking: Vec<(String, f32)> =
            if matches!(mode, SearchMode::Vector | SearchMode::Hybrid) {
                let vec_bytes: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
                // vec0 knn requires LIMIT (or `k = ?`) in the same SELECT that has the MATCH
                // clause, so run the knn as a subquery and join against meta outside.
                let sql = match filter_json {
                    Some(_) => schema.build_vec_knn_select_filtered().sql,
                    None => schema.build_vec_knn_select().sql,
                };
                Self::ranking(
                    conn,
                    &sql,
                    &vec_bytes,
                    candidate_limit,
                    filter_json.as_deref(),
                )?
            } else {
                Vec::new()
            };

        // --- Keyword rankings ---
        let kw_ranking: Vec<(String, f32)> =
            if matches!(mode, SearchMode::Keyword | SearchMode::Hybrid) {
                let q = keyword_query.unwrap();
                let sql = match filter_json {
                    Some(_) => schema.build_fts_bm25_select_filtered().sql,
                    None => schema.build_fts_bm25_select().sql,
                };
                Self::ranking(conn, &sql, &q, candidate_limit, filter_json.as_deref())?
            } else {
                Vec::new()
            };

        // For Hybrid, fuse once and KEEP the (id, score) pairs so the returned
        // score is the genuine RRF value, not a positional placeholder that
        // silently diverges from the Vector/Keyword arms' real scores.
        // `fuse_scored` already truncates to top_k, so no extra slicing needed.
        let hybrid_fused: Vec<(String, f32)> = match mode {
            SearchMode::Hybrid => {
                let vec_ids: Vec<String> = vec_ranking.iter().map(|(id, _)| id.clone()).collect();
                let kw_ids: Vec<String> = kw_ranking.iter().map(|(id, _)| id.clone()).collect();
                rrf::fuse_scored(&[vec_ids, kw_ids], top_k, rrf::DEFAULT_RRF_K)
            }
            _ => Vec::new(),
        };
        let ranked: Vec<(String, f32)> = match mode {
            SearchMode::Vector => vec_ranking,
            SearchMode::Keyword => kw_ranking,
            SearchMode::Hybrid => hybrid_fused,
        };
        // The Keyword arm's candidate LIMIT is at least 50, so cut to `top_k`.
        let ranked: Vec<(String, f32)> = ranked.into_iter().take(top_k).collect();

        if ranked.is_empty() {
            return Ok(Vec::new());
        }

        // Metadata lookup
        let mut stmt = conn
            .prepare(&schema.build_select_metadata_in(ranked.len()).sql)
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        let mut meta_map: std::collections::HashMap<String, serde_json::Value> = stmt
            .query_map(
                rusqlite::params_from_iter(ranked.iter().map(|(id, _)| id)),
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .map_err(|e| VectorError::Internal(e.to_string()))?
            .map(|row| {
                let (id, meta) = row.map_err(|e| VectorError::Internal(e.to_string()))?;
                let value = match meta {
                    Some(text) => {
                        serde_json::from_str::<serde_json::Value>(&text).map_err(|e| {
                            VectorError::Internal(format!(
                                "metadata of entry {id:?} is not JSON: {e}"
                            ))
                        })?
                    }
                    None => serde_json::Value::Null,
                };
                Ok((id, value))
            })
            .collect::<Result<_, VectorError>>()?;

        Ok(ranked
            .into_iter()
            .map(|(id, score)| VectorMatch {
                metadata: meta_map.remove(&id),
                id,
                score,
            })
            .collect())
    }

    /// Run one ranking statement: `?1` is the query (embedding bytes or FTS5
    /// query text), `?2` the candidate limit, and `?3` the serialized
    /// metadata filter when the statement is a `*_filtered` one. Rows are
    /// `(id, score)` in rank order.
    fn ranking(
        conn: &Connection,
        sql: &str,
        query: &dyn rusqlite::ToSql,
        limit: usize,
        filter_json: Option<&str>,
    ) -> Result<Vec<(String, f32)>, VectorError> {
        let limit = i64::try_from(limit)
            .map_err(|_| VectorError::Internal(format!("candidate limit {limit} overflows i64")))?;
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        let map_row =
            |row: &rusqlite::Row<'_>| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)? as f32));
        let rows = match filter_json {
            Some(f) => stmt.query_map(params![query, limit, f], map_row),
            None => stmt.query_map(params![query, limit], map_row),
        }
        .map_err(|e| VectorError::Internal(e.to_string()))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| VectorError::Internal(e.to_string()))
    }
}

/// Body of the [`METADATA_FILTER_FN`] SQL function: `(metadata, filter_json)`
/// → whether `metadata` satisfies the filter, per [`MetadataFilter::matches`].
/// Evaluating the filter's own predicate keeps a filtered search in
/// agreement with the filter's defined meaning by construction.
/// The filter argument is the same for every row of a statement, so it is
/// parsed once and cached as SQLite auxiliary data. A `NULL` metadata column
/// is absent metadata; text that is not JSON is an error, not a mismatch.
fn metadata_filter_fn(ctx: &Context<'_>) -> rusqlite::Result<bool> {
    let filter = ctx.get_or_create_aux(1, |value| -> Result<MetadataFilter, BoxError> {
        Ok(serde_json::from_str(value.as_str()?)?)
    })?;
    let metadata = match ctx.get_raw(0) {
        rusqlite::types::ValueRef::Null => None,
        value => Some(
            serde_json::from_str::<serde_json::Value>(
                value
                    .as_str()
                    .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?,
            )
            .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?,
        ),
    };
    Ok(filter.matches(metadata.as_ref()))
}

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(name: &str, keyword_search: bool) -> VectorIndexConfig {
        VectorIndexConfig {
            name: name.into(),
            model: "bge-m3".into(),
            dimensions: 1024,
            metric: DistanceMetric::Cosine,
            keyword_search,
        }
    }

    /// Scalar assertion query executed on the connection worker — the
    /// test-side replacement for the retired direct `svc.db.lock()` access.
    async fn query_i64_for_tests(svc: &SqliteVecService, sql: &str) -> i64 {
        let sql = sql.to_string();
        svc.worker
            .run(move |conn| conn.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap())
            .await
            .expect("vector worker alive")
    }

    #[tokio::test]
    async fn create_index_vector_only() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(cfg("docs", false)).await.unwrap();
        let count = query_i64_for_tests(
            &svc,
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('docs_vec','docs_meta')",
        )
        .await;
        assert_eq!(count, 2);
        let fts_exists = query_i64_for_tests(
            &svc,
            "SELECT COUNT(*) FROM sqlite_master WHERE name='docs_fts'",
        )
        .await;
        assert_eq!(
            fts_exists, 0,
            "FTS table must NOT exist when keyword_search=false"
        );
    }

    #[tokio::test]
    async fn create_index_with_keyword_search() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(cfg("docs", true)).await.unwrap();
        let fts_exists = query_i64_for_tests(
            &svc,
            "SELECT COUNT(*) FROM sqlite_master WHERE name='docs_fts'",
        )
        .await;
        assert_eq!(fts_exists, 1);
    }

    #[tokio::test]
    async fn create_index_duplicate_fails() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(cfg("docs", false)).await.unwrap();
        let err = svc.create_index(cfg("docs", false)).await.unwrap_err();
        assert!(matches!(err, VectorError::IndexAlreadyExists(_)));
    }

    #[tokio::test]
    async fn delete_index_removes_tables() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(cfg("docs", true)).await.unwrap();
        svc.delete_index("docs").await.unwrap();
        let n = query_i64_for_tests(
            &svc,
            "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'docs_%'",
        )
        .await;
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn delete_missing_index_errors() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        let err = svc.delete_index("nope").await.unwrap_err();
        assert!(matches!(err, VectorError::IndexNotFound(_)));
    }

    fn entry(id: &str, v: Vec<f32>, text: Option<&str>) -> VectorEntry {
        VectorEntry {
            id: id.into(),
            vector: v,
            metadata: Some(serde_json::json!({ "source": "test" })),
            text: text.map(String::from),
        }
    }

    #[tokio::test]
    async fn upsert_vector_only() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(VectorIndexConfig {
            name: "docs".into(),
            model: "m".into(),
            dimensions: 3,
            metric: DistanceMetric::Cosine,
            keyword_search: false,
        })
        .await
        .unwrap();

        svc.upsert(
            "docs",
            vec![
                entry("a", vec![1.0, 0.0, 0.0], None),
                entry("b", vec![0.0, 1.0, 0.0], None),
            ],
        )
        .await
        .unwrap();

        let n = query_i64_for_tests(&svc, "SELECT COUNT(*) FROM docs_meta").await;
        assert_eq!(n, 2);
    }

    #[tokio::test]
    async fn upsert_requires_text_when_keyword_search() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(VectorIndexConfig {
            name: "docs".into(),
            model: "m".into(),
            dimensions: 3,
            metric: DistanceMetric::Cosine,
            keyword_search: true,
        })
        .await
        .unwrap();

        let err = svc
            .upsert("docs", vec![entry("a", vec![1.0, 0.0, 0.0], None)])
            .await
            .unwrap_err();
        assert!(matches!(err, VectorError::TextRequired));
    }

    #[tokio::test]
    async fn upsert_replaces_existing_id() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(VectorIndexConfig {
            name: "docs".into(),
            model: "m".into(),
            dimensions: 3,
            metric: DistanceMetric::Cosine,
            keyword_search: false,
        })
        .await
        .unwrap();
        svc.upsert("docs", vec![entry("a", vec![1.0, 0.0, 0.0], None)])
            .await
            .unwrap();
        svc.upsert("docs", vec![entry("a", vec![0.0, 1.0, 0.0], None)])
            .await
            .unwrap();
        let n = query_i64_for_tests(&svc, "SELECT COUNT(*) FROM docs_meta").await;
        assert_eq!(n, 1);
        let n_vec = query_i64_for_tests(&svc, "SELECT COUNT(*) FROM docs_vec").await;
        assert_eq!(n_vec, 1);
    }

    #[tokio::test]
    async fn query_vector_mode() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(VectorIndexConfig {
            name: "docs".into(),
            model: "m".into(),
            dimensions: 3,
            metric: DistanceMetric::Cosine,
            keyword_search: false,
        })
        .await
        .unwrap();
        svc.upsert(
            "docs",
            vec![
                entry("a", vec![1.0, 0.0, 0.0], None),
                entry("b", vec![0.0, 1.0, 0.0], None),
                entry("c", vec![0.0, 0.0, 1.0], None),
            ],
        )
        .await
        .unwrap();

        let hits = svc
            .query(
                "docs",
                vec![1.0, 0.0, 0.0],
                2,
                None,
                SearchMode::Vector,
                None,
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "a");
    }

    #[tokio::test]
    async fn query_keyword_mode_requires_keyword_search() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(VectorIndexConfig {
            name: "docs".into(),
            model: "m".into(),
            dimensions: 3,
            metric: DistanceMetric::Cosine,
            keyword_search: false,
        })
        .await
        .unwrap();
        let err = svc
            .query(
                "docs",
                vec![0.0; 3],
                5,
                None,
                SearchMode::Keyword,
                Some("cat".into()),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, VectorError::KeywordSearchNotEnabled));
    }

    #[tokio::test]
    async fn query_hybrid_mode() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(VectorIndexConfig {
            name: "docs".into(),
            model: "m".into(),
            dimensions: 3,
            metric: DistanceMetric::Cosine,
            keyword_search: true,
        })
        .await
        .unwrap();
        svc.upsert(
            "docs",
            vec![
                entry("a", vec![1.0, 0.0, 0.0], Some("cats are soft")),
                entry("b", vec![0.0, 1.0, 0.0], Some("dogs bark loud")),
                entry("c", vec![0.0, 0.0, 1.0], Some("cats can climb")),
            ],
        )
        .await
        .unwrap();
        let hits = svc
            .query(
                "docs",
                vec![0.9, 0.1, 0.1],
                3,
                None,
                SearchMode::Hybrid,
                Some("cats".into()),
            )
            .await
            .unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].id, "a");
    }

    #[tokio::test]
    async fn query_filter_excludes_non_matching() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(VectorIndexConfig {
            name: "docs".into(),
            model: "m".into(),
            dimensions: 3,
            metric: DistanceMetric::Cosine,
            keyword_search: false,
        })
        .await
        .unwrap();
        let e1 = VectorEntry {
            id: "a".into(),
            vector: vec![1.0, 0.0, 0.0],
            metadata: Some(serde_json::json!({"tag": "x"})),
            text: None,
        };
        let e2 = VectorEntry {
            id: "b".into(),
            vector: vec![0.9, 0.1, 0.0],
            metadata: Some(serde_json::json!({"tag": "y"})),
            text: None,
        };
        svc.upsert("docs", vec![e1, e2]).await.unwrap();
        let mut filter = MetadataFilter::default();
        filter.equals.insert("tag".into(), serde_json::json!("y"));
        let hits = svc
            .query(
                "docs",
                vec![1.0, 0.0, 0.0],
                5,
                Some(filter),
                SearchMode::Vector,
                None,
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "b");
    }

    #[tokio::test]
    async fn delete_removes_from_all_tables() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(VectorIndexConfig {
            name: "docs".into(),
            model: "m".into(),
            dimensions: 3,
            metric: DistanceMetric::Cosine,
            keyword_search: true,
        })
        .await
        .unwrap();
        svc.upsert(
            "docs",
            vec![
                entry("a", vec![1.0, 0.0, 0.0], Some("one")),
                entry("b", vec![0.0, 1.0, 0.0], Some("two")),
            ],
        )
        .await
        .unwrap();
        svc.delete("docs", vec!["a".into()]).await.unwrap();
        assert_eq!(svc.count("docs").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn count_empty_index_is_zero() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(VectorIndexConfig {
            name: "docs".into(),
            model: "m".into(),
            dimensions: 3,
            metric: DistanceMetric::Cosine,
            keyword_search: false,
        })
        .await
        .unwrap();
        assert_eq!(svc.count("docs").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn list_indexes_matches_prefix_literally() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(cfg("foo_bar", false)).await.unwrap();
        svc.create_index(cfg("fooxbar", false)).await.unwrap();
        // `_` in the prefix must match literally, not as a LIKE wildcard.
        let got = svc.list_indexes("foo_").await.unwrap();
        assert_eq!(got, vec!["foo_bar".to_string()]);
        let all = svc.list_indexes("foo").await.unwrap();
        assert_eq!(all, vec!["foo_bar".to_string(), "fooxbar".to_string()]);
    }

    #[tokio::test]
    async fn describe_index_reports_columns_and_fts() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(cfg("docs", true)).await.unwrap();
        let desc = svc.describe_index("docs").await.unwrap();
        assert!(desc.exists);
        assert!(desc.keyword_search);
        let names: Vec<&str> = desc.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "rowid", "metadata", "text"]);
        assert_eq!(desc.columns[0].sql_type, "TEXT");
        assert_eq!(desc.columns[1].sql_type, "INTEGER");

        let vector_only_desc = {
            svc.create_index(cfg("plain", false)).await.unwrap();
            svc.describe_index("plain").await.unwrap()
        };
        assert!(vector_only_desc.exists);
        assert!(!vector_only_desc.keyword_search);

        let missing = svc.describe_index("nope").await.unwrap();
        assert!(!missing.exists);
        assert!(missing.columns.is_empty());
        assert!(!missing.keyword_search);
    }

    #[tokio::test]
    async fn list_ids_filters_by_metadata_equality() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(cfg("docs", false)).await.unwrap();
        let e = |id: &str, doc: &str, page: i64| VectorEntry {
            id: id.into(),
            vector: vec![0.1; 1024],
            metadata: Some(serde_json::json!({ "document_id": doc, "page": page })),
            text: None,
        };
        svc.upsert(
            "docs",
            vec![e("a", "d1", 1), e("b", "d1", 2), e("c", "d2", 1)],
        )
        .await
        .unwrap();

        let mut filter = MetadataFilter::default();
        filter
            .equals
            .insert("document_id".into(), serde_json::json!("d1"));
        let mut ids = svc.list_ids("docs", filter).await.unwrap();
        ids.sort();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);

        // Numeric equality binds as a number, and multiple conditions AND.
        let mut filter = MetadataFilter::default();
        filter
            .equals
            .insert("document_id".into(), serde_json::json!("d1"));
        filter.equals.insert("page".into(), serde_json::json!(2));
        let ids = svc.list_ids("docs", filter).await.unwrap();
        assert_eq!(ids, vec!["b".to_string()]);
    }

    #[tokio::test]
    async fn list_ids_rejects_empty_and_non_scalar_filters() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(cfg("docs", false)).await.unwrap();

        let err = svc
            .list_ids("docs", MetadataFilter::default())
            .await
            .unwrap_err();
        assert!(matches!(err, VectorError::InvalidMetadataFilter(_)));

        let mut filter = MetadataFilter::default();
        filter.equals.insert("flag".into(), serde_json::json!(true));
        let err = svc.list_ids("docs", filter).await.unwrap_err();
        assert!(matches!(err, VectorError::InvalidMetadataFilter(_)));
    }

    #[tokio::test]
    async fn list_ids_missing_index_is_not_found() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        let mut filter = MetadataFilter::default();
        filter.equals.insert("k".into(), serde_json::json!("v"));
        let err = svc.list_ids("nope", filter).await.unwrap_err();
        assert!(matches!(err, VectorError::IndexNotFound(_)));
    }

    fn dims3(name: &str, keyword_search: bool) -> VectorIndexConfig {
        VectorIndexConfig {
            name: name.into(),
            model: "m".into(),
            dimensions: 3,
            metric: DistanceMetric::Cosine,
            keyword_search,
        }
    }

    fn owned(id: String, owner: &str, v: Vec<f32>, text: Option<&str>) -> VectorEntry {
        VectorEntry {
            id,
            vector: v,
            metadata: Some(serde_json::json!({ "owner": owner })),
            text: text.map(String::from),
        }
    }

    fn owner_filter(owner: &str) -> MetadataFilter {
        let mut filter = MetadataFilter::default();
        filter
            .equals
            .insert("owner".into(), serde_json::json!(owner));
        filter
    }

    /// 100 entries: the 50 nearest to the query belong to `u2`, `u1`'s 50 lie
    /// further out. A `u1`-filtered top-5 must be `u1`'s five nearest, not
    /// the (empty) `u1` subset of the unfiltered top-5.
    #[tokio::test]
    async fn vector_query_filter_returns_top_k_of_the_filtered_set() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(dims3("docs", false)).await.unwrap();
        let mut entries = Vec::new();
        for i in 0..50 {
            let near = vec![1.0, 0.001 * i as f32, 0.0];
            let far = vec![0.0, 1.0, 0.01 * i as f32];
            entries.push(owned(format!("u2-{i:02}"), "u2", near, None));
            entries.push(owned(format!("u1-{i:02}"), "u1", far, None));
        }
        svc.upsert("docs", entries).await.unwrap();

        let hits = svc
            .query(
                "docs",
                vec![1.0, 0.0, 0.0],
                5,
                Some(owner_filter("u1")),
                SearchMode::Vector,
                None,
            )
            .await
            .unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, vec!["u1-00", "u1-01", "u1-02", "u1-03", "u1-04"]);
        assert!(hits
            .iter()
            .all(|h| h.metadata == Some(serde_json::json!({ "owner": "u1" }))));
    }

    /// Keyword and hybrid rankings take at least 50 candidates; 60 `u2`
    /// entries outrank every `u1` entry on bm25 (and on distance), so a
    /// filter applied after that cut would leave nothing for `u1`.
    #[tokio::test]
    async fn keyword_and_hybrid_query_filter_returns_top_k_of_the_filtered_set() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(dims3("docs", true)).await.unwrap();
        let mut entries = Vec::new();
        for i in 0..60 {
            entries.push(owned(
                format!("u2-{i:02}"),
                "u2",
                vec![1.0, 0.001 * i as f32, 0.0],
                Some("cats cats cats"),
            ));
        }
        for i in 0..10 {
            entries.push(owned(
                format!("u1-{i:02}"),
                "u1",
                vec![0.0, 1.0, 0.01 * i as f32],
                Some("cats sit next to many dogs and birds in the long grass all day"),
            ));
        }
        svc.upsert("docs", entries).await.unwrap();

        for mode in [SearchMode::Keyword, SearchMode::Hybrid] {
            let hits = svc
                .query(
                    "docs",
                    vec![1.0, 0.0, 0.0],
                    5,
                    Some(owner_filter("u1")),
                    mode,
                    Some("cats".into()),
                )
                .await
                .unwrap();
            assert_eq!(hits.len(), 5, "{mode:?}: {hits:?}");
            assert!(
                hits.iter().all(|h| h.id.starts_with("u1-")),
                "{mode:?}: {hits:?}"
            );
        }
    }

    /// The in-SQL filter is `MetadataFilter::matches` itself, so it keeps
    /// the filter's typed, dot-path, subtree and absent-metadata rules.
    /// Guard test: these entries all fit in the unfiltered top-k, so it
    /// passes whether the filter runs before or after the cut.
    #[tokio::test]
    async fn query_filter_follows_metadata_filter_semantics() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(dims3("docs", false)).await.unwrap();
        let e = |id: &str, metadata: Option<serde_json::Value>| VectorEntry {
            id: id.into(),
            vector: vec![1.0, 0.0, 0.0],
            metadata,
            text: None,
        };
        svc.upsert(
            "docs",
            vec![
                e(
                    "int",
                    Some(serde_json::json!({ "n": 1, "doc": { "rev": 2 } })),
                ),
                e("str", Some(serde_json::json!({ "n": "1" }))),
                e("bool", Some(serde_json::json!({ "n": true }))),
                e("none", None),
            ],
        )
        .await
        .unwrap();

        let ids_for = |path: &str, value: serde_json::Value| {
            let mut filter = MetadataFilter::default();
            filter.equals.insert(path.into(), value);
            let svc = &svc;
            async move {
                let mut ids: Vec<String> = svc
                    .query(
                        "docs",
                        vec![1.0, 0.0, 0.0],
                        10,
                        Some(filter),
                        SearchMode::Vector,
                        None,
                    )
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|h| h.id)
                    .collect();
                ids.sort();
                ids
            }
        };
        assert_eq!(ids_for("n", serde_json::json!(1)).await, vec!["int"]);
        assert_eq!(ids_for("n", serde_json::json!("1")).await, vec!["str"]);
        assert_eq!(ids_for("n", serde_json::json!(true)).await, vec!["bool"]);
        assert_eq!(ids_for("doc.rev", serde_json::json!(2)).await, vec!["int"]);
        assert_eq!(
            ids_for("doc", serde_json::json!({ "rev": 2 })).await,
            vec!["int"]
        );
        assert!(ids_for("n.x", serde_json::json!(1)).await.is_empty());
    }

    /// A rowid that cannot be read must fail the delete, not be skipped:
    /// skipping it deletes the `_meta` row but leaves its `_vec` row behind
    /// with nothing pointing at it.
    #[tokio::test]
    async fn delete_fails_on_an_unreadable_rowid_instead_of_orphaning_the_vector() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(dims3("docs", false)).await.unwrap();
        svc.upsert(
            "docs",
            vec![
                entry("a", vec![1.0, 0.0, 0.0], None),
                entry("b", vec![0.0, 1.0, 0.0], None),
            ],
        )
        .await
        .unwrap();
        // SQLite column types are advisory: store a rowid that is not an integer.
        svc.worker
            .run(|conn| {
                conn.execute("UPDATE docs_meta SET rowid = 'x' WHERE id = 'a'", [])
                    .unwrap();
            })
            .await
            .expect("vector worker alive");

        let err = svc.delete("docs", vec!["a".into()]).await.unwrap_err();
        assert!(matches!(err, VectorError::Internal(_)), "{err:?}");
        assert_eq!(
            query_i64_for_tests(&svc, "SELECT COUNT(*) FROM docs_meta").await,
            2
        );
        assert_eq!(
            query_i64_for_tests(&svc, "SELECT COUNT(*) FROM docs_vec").await,
            2
        );
    }

    /// A fresh path for an on-disk database, removed (with its WAL files) on drop.
    struct TempDb(std::path::PathBuf);

    impl TempDb {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "wafer-vector-{tag}-{}-{nanos}.db",
                std::process::id()
            )))
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let mut path = self.0.clone().into_os_string();
                path.push(suffix);
                let _ = std::fs::remove_file(path);
            }
        }
    }

    /// Another connection to the same file takes the write lock, signals,
    /// holds the lock for `hold`, then commits. Stands in for the database
    /// service writing to the file the vector service shares.
    fn hold_write_lock(
        path: &std::path::Path,
        hold: std::time::Duration,
    ) -> std::thread::JoinHandle<()> {
        let path = path.to_path_buf();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let other = Connection::open(&path).unwrap();
            other
                .execute_batch(
                    "BEGIN IMMEDIATE; \
                     CREATE TABLE IF NOT EXISTS other(n INTEGER); \
                     INSERT INTO other(n) VALUES (1);",
                )
                .unwrap();
            ready_tx.send(()).unwrap();
            std::thread::sleep(hold);
            other.execute_batch("COMMIT;").unwrap();
        });
        ready_rx.recv().unwrap();
        writer
    }

    /// The vector service shares its database file with another writer (the
    /// database service). While that writer holds the lock, a vector write
    /// must wait for it through the busy handler and then succeed, not fail
    /// at once with SQLITE_BUSY.
    #[tokio::test]
    async fn writes_wait_for_another_connection_holding_the_write_lock() {
        let db = TempDb::new("busy");
        let probe = Connection::open_in_memory().unwrap();
        ensure_vec_loaded(&probe).unwrap();
        let conn = Connection::open(&db.0).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let svc = SqliteVecService::new(conn).unwrap();
        svc.create_index(dims3("docs", false)).await.unwrap();
        svc.upsert("docs", vec![entry("a", vec![1.0, 0.0, 0.0], None)])
            .await
            .unwrap();

        let hold = std::time::Duration::from_millis(300);

        let writer = hold_write_lock(&db.0, hold);
        let upsert = svc
            .upsert("docs", vec![entry("b", vec![0.0, 1.0, 0.0], None)])
            .await;
        writer.join().unwrap();
        upsert.expect("upsert waits for the other writer, then commits");
        assert_eq!(svc.count("docs").await.unwrap(), 2);

        let writer = hold_write_lock(&db.0, hold);
        let delete = svc.delete("docs", vec!["a".into()]).await;
        writer.join().unwrap();
        delete.expect("delete waits for the other writer, then commits");
        assert_eq!(svc.count("docs").await.unwrap(), 1);
        drop(svc);
    }

    // --- rename_index ---------------------------------------------------

    const LEGACY: &str = "my_org__vector__Docs";
    const LOWER: &str = "my_org__vector__docs";

    /// Build an index named `LEGACY` the way the service did before index
    /// names had to be lowercase: the same three tables, created directly
    /// because the service no longer accepts the name. Rowids are sparse
    /// (1, 5, 9) as deletes leave them, so a move that renumbered the vec0
    /// rows would detach vectors from their `_meta` rows.
    async fn legacy_index(svc: &SqliteVecService, keyword_search: bool) {
        svc.worker
            .run(move |conn| {
                conn.execute_batch(
                    "CREATE VIRTUAL TABLE my_org__vector__Docs_vec USING vec0(embedding float[3]);
                     CREATE TABLE my_org__vector__Docs_meta(
                        id TEXT PRIMARY KEY,
                        rowid INTEGER NOT NULL,
                        metadata TEXT,
                        text TEXT
                     );",
                )
                .unwrap();
                if keyword_search {
                    conn.execute_batch(
                        "CREATE VIRTUAL TABLE my_org__vector__Docs_fts USING fts5(id UNINDEXED, text);",
                    )
                    .unwrap();
                }
                for (rowid, id, v, tag, text) in [
                    (1_i64, "a", [1.0_f32, 0.0, 0.0], "x", "cats are soft"),
                    (5, "b", [0.0, 1.0, 0.0], "y", "dogs bark loud"),
                    (9, "c", [0.0, 0.0, 1.0], "z", "fish swim deep"),
                ] {
                    let bytes: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
                    conn.execute(
                        "INSERT INTO my_org__vector__Docs_vec(rowid, embedding) VALUES (?1, ?2)",
                        params![rowid, bytes],
                    )
                    .unwrap();
                    conn.execute(
                        "INSERT INTO my_org__vector__Docs_meta(id, rowid, metadata, text) \
                         VALUES (?1, ?2, ?3, ?4)",
                        params![id, rowid, format!(r#"{{"tag":"{tag}"}}"#), text],
                    )
                    .unwrap();
                    if keyword_search {
                        conn.execute(
                            "INSERT INTO my_org__vector__Docs_fts(id, text) VALUES (?1, ?2)",
                            params![id, text],
                        )
                        .unwrap();
                    }
                }
            })
            .await
            .expect("vector worker alive");
    }

    /// Every table name, sorted, excluding vec0 / FTS5 shadow tables and
    /// SQLite's own (`sqlite_sequence`, which vec0 creates).
    async fn catalog(svc: &SqliteVecService) -> Vec<String> {
        svc.worker
            .run(|conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT name FROM sqlite_master WHERE type='table' \
                         AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
                         AND name NOT LIKE '%\\_vec\\_%' ESCAPE '\\' \
                         AND name NOT LIKE '%\\_fts\\_%' ESCAPE '\\' ORDER BY name",
                    )
                    .unwrap();
                stmt.query_map([], |r| r.get::<_, String>(0))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<String>>>()
                    .unwrap()
            })
            .await
            .expect("vector worker alive")
    }

    /// Catalog entries of any kind — shadow tables included — still spelled
    /// with the legacy name or the staging stem (`GLOB` is case-sensitive).
    /// vec0 has no `xRename`, so an `ALTER TABLE` of the vec table would
    /// leave its shadow tables here under `…Docs_vec_*`.
    async fn leftovers(svc: &SqliteVecService) -> i64 {
        query_i64_for_tests(
            svc,
            "SELECT COUNT(*) FROM sqlite_master WHERE name GLOB '*Docs*' OR name GLOB '*-rename*'",
        )
        .await
    }

    async fn nearest(svc: &SqliteVecService, index: &str, v: Vec<f32>) -> VectorMatch {
        svc.query(index, v, 1, None, SearchMode::Vector, None)
            .await
            .unwrap()
            .remove(0)
    }

    /// Guard (passes before and after this op exists): the service refuses
    /// the legacy name, and the lowercase name does not find the tables,
    /// so a legacy index is unreachable until it is renamed.
    #[tokio::test]
    async fn a_legacy_index_is_unreachable_by_either_spelling() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        legacy_index(&svc, true).await;
        assert!(matches!(
            svc.count(LEGACY).await.unwrap_err(),
            VectorError::InvalidIndexName(_)
        ));
        assert!(matches!(
            svc.count(LOWER).await.unwrap_err(),
            VectorError::IndexNotFound(_)
        ));
    }

    /// The moved index is the old one: every vector still ranks against its
    /// own id and metadata, keyword search still finds the old text, and
    /// the index takes new writes and deletes. Nothing is left under the
    /// old or the staging names.
    #[tokio::test]
    async fn rename_index_moves_a_legacy_index_with_its_data() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        legacy_index(&svc, true).await;

        svc.rename_index(LEGACY, LOWER).await.unwrap();

        assert_eq!(svc.count(LOWER).await.unwrap(), 3);
        for (v, id, tag) in [
            (vec![1.0, 0.0, 0.0], "a", "x"),
            (vec![0.0, 1.0, 0.0], "b", "y"),
            (vec![0.0, 0.0, 1.0], "c", "z"),
        ] {
            let hit = nearest(&svc, LOWER, v).await;
            assert_eq!(hit.id, id);
            assert_eq!(hit.metadata, Some(serde_json::json!({ "tag": tag })));
        }
        let kw = svc
            .query(
                LOWER,
                vec![0.0; 3],
                5,
                None,
                SearchMode::Keyword,
                Some("dogs".into()),
            )
            .await
            .unwrap();
        assert_eq!(kw.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["b"]);
        let desc = svc.describe_index(LOWER).await.unwrap();
        assert!(desc.exists && desc.keyword_search);

        // New writes continue after the highest moved rowid.
        svc.upsert(
            LOWER,
            vec![entry("d", vec![0.5, 0.5, 0.0], Some("birds sing"))],
        )
        .await
        .unwrap();
        assert_eq!(nearest(&svc, LOWER, vec![0.5, 0.5, 0.0]).await.id, "d");
        assert_eq!(
            query_i64_for_tests(
                &svc,
                "SELECT rowid FROM my_org__vector__docs_meta WHERE id = 'd'"
            )
            .await,
            10
        );
        svc.delete(LOWER, vec!["a".into()]).await.unwrap();
        assert_eq!(svc.count(LOWER).await.unwrap(), 3);
        assert_eq!(nearest(&svc, LOWER, vec![1.0, 0.0, 0.0]).await.id, "d");

        assert_eq!(
            catalog(&svc).await,
            [
                "my_org__vector__docs_fts",
                "my_org__vector__docs_meta",
                "my_org__vector__docs_vec"
            ]
        );
        assert_eq!(svc.list_indexes("my_org__vector__").await.unwrap(), [LOWER]);
        assert_eq!(leftovers(&svc).await, 0);
    }

    #[tokio::test]
    async fn rename_index_moves_a_vector_only_index() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        legacy_index(&svc, false).await;

        svc.rename_index(LEGACY, LOWER).await.unwrap();

        assert_eq!(nearest(&svc, LOWER, vec![0.0, 1.0, 0.0]).await.id, "b");
        assert!(!svc.describe_index(LOWER).await.unwrap().keyword_search);
        assert_eq!(
            catalog(&svc).await,
            ["my_org__vector__docs_meta", "my_org__vector__docs_vec"]
        );
        assert_eq!(leftovers(&svc).await, 0);
    }

    /// Once moved, `from` is gone: a second run reports it missing, which a
    /// startup migration reads as done when `to` exists.
    #[tokio::test]
    async fn rename_index_twice_reports_the_source_missing() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        legacy_index(&svc, true).await;
        svc.rename_index(LEGACY, LOWER).await.unwrap();

        match svc.rename_index(LEGACY, LOWER).await.unwrap_err() {
            VectorError::IndexNotFound(name) => assert_eq!(name, LEGACY),
            other => panic!("expected IndexNotFound, got {other:?}"),
        }
        assert!(svc.describe_index(LOWER).await.unwrap().exists);
        assert_eq!(svc.count(LOWER).await.unwrap(), 3);
    }

    /// `from` is matched exactly: another spelling of the name is a
    /// different index, even though SQLite would resolve it to the same
    /// tables.
    #[tokio::test]
    async fn rename_index_matches_the_source_name_exactly() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        legacy_index(&svc, true).await;
        match svc
            .rename_index("my_org__vector__DOCS", LOWER)
            .await
            .unwrap_err()
        {
            VectorError::IndexNotFound(name) => assert_eq!(name, "my_org__vector__DOCS"),
            other => panic!("expected IndexNotFound, got {other:?}"),
        }
        assert_eq!(
            catalog(&svc).await,
            [
                "my_org__vector__Docs_fts",
                "my_org__vector__Docs_meta",
                "my_org__vector__Docs_vec"
            ]
        );
    }

    /// SQLite cannot hold two spellings of one table name, so `to` can only
    /// be occupied by a table `from` does not have: here an FTS table beside
    /// a vector-only legacy index. Moving the index onto it would silently
    /// give it someone else's keyword search, so the rename is refused and
    /// nothing moves.
    #[tokio::test]
    async fn rename_index_refuses_an_occupied_target() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        legacy_index(&svc, false).await;
        svc.worker
            .run(|conn| {
                conn.execute_batch(
                    "CREATE VIRTUAL TABLE my_org__vector__DOCS_fts USING fts5(id UNINDEXED, text);",
                )
                .unwrap();
            })
            .await
            .expect("vector worker alive");

        match svc.rename_index(LEGACY, LOWER).await.unwrap_err() {
            VectorError::IndexAlreadyExists(name) => assert_eq!(name, LOWER),
            other => panic!("expected IndexAlreadyExists, got {other:?}"),
        }
        assert_eq!(
            catalog(&svc).await,
            [
                "my_org__vector__DOCS_fts",
                "my_org__vector__Docs_meta",
                "my_org__vector__Docs_vec"
            ]
        );
    }

    /// A move that fails part-way leaves the index exactly as it was: the
    /// staging meta table is occupied, so the rename fails after the vec0
    /// rows have been moved, and the transaction puts them back.
    #[tokio::test]
    async fn rename_index_failing_part_way_changes_nothing() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        legacy_index(&svc, true).await;
        svc.worker
            .run(|conn| {
                conn.execute_batch(r#"CREATE TABLE "my_org__vector__docs-rename_meta"(x);"#)
                    .unwrap();
            })
            .await
            .expect("vector worker alive");

        let err = svc.rename_index(LEGACY, LOWER).await.unwrap_err();
        assert!(matches!(err, VectorError::Internal(_)), "{err:?}");
        assert_eq!(
            catalog(&svc).await,
            [
                "my_org__vector__Docs_fts",
                "my_org__vector__Docs_meta",
                "my_org__vector__Docs_vec",
                "my_org__vector__docs-rename_meta"
            ]
        );
        assert_eq!(
            query_i64_for_tests(&svc, "SELECT COUNT(*) FROM my_org__vector__Docs_vec").await,
            3
        );
    }

    #[tokio::test]
    async fn rename_index_refuses_anything_but_a_legacy_spelling() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        legacy_index(&svc, true).await;
        for (from, to) in [
            (LOWER, LOWER),
            (LEGACY, "my_org__vector__other"),
            (LEGACY, LEGACY),
            ("my_org__vector__Docs_x", LOWER),
        ] {
            let err = svc.rename_index(from, to).await.unwrap_err();
            assert!(
                matches!(err, VectorError::InvalidRename { .. }),
                "{from} -> {to}: {err:?}"
            );
        }
    }
}
