//! SQLite-backed VectorService using sqlite-vec for similarity search
//! and FTS5 for optional keyword search.

use parking_lot::Mutex;
use rusqlite::{params, Connection};
use wafer_core::interfaces::vector::{
    rrf,
    service::{
        DistanceMetric, MetadataFilter, SearchMode, VectorEntry, VectorError, VectorIndexConfig,
        VectorMatch, VectorService,
    },
};
use wafer_sql_utils::vector::VectorIndexSchema;

use crate::ensure_vec_loaded;

/// `VectorService` backed by SQLite + `sqlite-vec` (`vec0` virtual
/// tables) for ANN search and FTS5 for keyword search. A single pooled
/// `rusqlite::Connection` is guarded by a `Mutex`; callers are expected
/// to register the `sqlite-vec` auto-extension before opening the
/// connection (see [`crate::ensure_vec_loaded`]).
pub struct SqliteVecService {
    db: Mutex<Connection>,
}

impl SqliteVecService {
    /// Wrap an existing `rusqlite::Connection` that already has the
    /// `sqlite-vec` extension loaded. Used by `solobase-core` to bind a
    /// shared on-disk DB to the vector service.
    pub fn new(db: Connection) -> Self {
        Self { db: Mutex::new(db) }
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
        Ok(Self::new(Connection::open_in_memory()?))
    }

    fn index_exists(conn: &Connection, schema: &VectorIndexSchema) -> Result<bool, VectorError> {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                params![&schema.vec_table],
                |row| row.get(0),
            )
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        Ok(exists)
    }

    fn has_keyword_search(
        conn: &Connection,
        schema: &VectorIndexSchema,
    ) -> Result<bool, VectorError> {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                params![&schema.fts_table],
                |row| row.get(0),
            )
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        Ok(exists)
    }
}

#[async_trait::async_trait]
impl VectorService for SqliteVecService {
    async fn create_index(&self, config: VectorIndexConfig) -> Result<(), VectorError> {
        let conn = self.db.lock();
        ensure_vec_loaded(&conn).map_err(|e| VectorError::Internal(e.to_string()))?;
        let schema = VectorIndexSchema::new(&config.name);
        if Self::index_exists(&conn, &schema)? {
            return Err(VectorError::IndexAlreadyExists(config.name));
        }
        // All 3 metric variants are accepted — sqlite-vec is distance-agnostic at storage;
        // it operates as cosine distance in SQL queries regardless of the stored metric tag.
        let _ = match config.metric {
            DistanceMetric::Cosine | DistanceMetric::Euclidean | DistanceMetric::DotProduct => (),
        };
        conn.execute_batch(&schema.build_create_vec_and_meta(config.dimensions))
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        if config.keyword_search {
            conn.execute_batch(&schema.build_create_fts())
                .map_err(|e| VectorError::Internal(e.to_string()))?;
        }
        Ok(())
    }

    async fn delete_index(&self, name: &str) -> Result<(), VectorError> {
        let conn = self.db.lock();
        ensure_vec_loaded(&conn).map_err(|e| VectorError::Internal(e.to_string()))?;
        let schema = VectorIndexSchema::new(name);
        if !Self::index_exists(&conn, &schema)? {
            return Err(VectorError::IndexNotFound(name.to_string()));
        }
        for stmt in schema.build_drop_all() {
            conn.execute(&stmt, [])
                .map_err(|e| VectorError::Internal(e.to_string()))?;
        }
        Ok(())
    }

    async fn upsert(&self, index: &str, entries: Vec<VectorEntry>) -> Result<(), VectorError> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut conn = self.db.lock();
        ensure_vec_loaded(&conn).map_err(|e| VectorError::Internal(e.to_string()))?;
        let schema = VectorIndexSchema::new(index);
        if !Self::index_exists(&conn, &schema)? {
            return Err(VectorError::IndexNotFound(index.to_string()));
        }
        let has_kw = Self::has_keyword_search(&conn, &schema)?;
        for e in &entries {
            if has_kw && e.text.is_none() {
                return Err(VectorError::TextRequired);
            }
        }

        let select_rowid_sql = schema.build_select_rowid_by_id();
        let delete_vec_sql = schema.build_delete_vec_by_rowid();
        let insert_meta_sql = schema.build_insert_meta_autoinc();
        let insert_vec_sql = schema.build_insert_vec();
        let update_meta_sql = schema.build_update_meta();
        let delete_fts_sql = schema.build_delete_fts_by_id();
        let insert_fts_sql = schema.build_insert_fts();

        let tx = conn
            .transaction()
            .map_err(|e| VectorError::Internal(e.to_string()))?;

        for e in entries {
            let meta_json = e
                .metadata
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "{}".into());

            // Find existing rowid (re-upsert path) or create a new one.
            let rowid: Option<i64> = tx
                .query_row(&select_rowid_sql, params![&e.id], |r| r.get(0))
                .ok();
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

    async fn query(
        &self,
        index: &str,
        vector: Vec<f32>,
        top_k: usize,
        filter: Option<MetadataFilter>,
        mode: SearchMode,
        keyword_query: Option<String>,
    ) -> Result<Vec<VectorMatch>, VectorError> {
        let conn = self.db.lock();
        ensure_vec_loaded(&conn).map_err(|e| VectorError::Internal(e.to_string()))?;
        let schema = VectorIndexSchema::new(index);
        if !Self::index_exists(&conn, &schema)? {
            return Err(VectorError::IndexNotFound(index.to_string()));
        }
        let has_kw = Self::has_keyword_search(&conn, &schema)?;
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

        // --- Vector rankings ---
        let vec_ranking: Vec<(String, f32)> =
            if matches!(mode, SearchMode::Vector | SearchMode::Hybrid) {
                let vec_bytes: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
                // vec0 knn requires LIMIT (or `k = ?`) in the same SELECT that has the MATCH
                // clause, so run the knn as a subquery and join against meta outside.
                let mut stmt = conn
                    .prepare(&schema.build_vec_knn_select())
                    .map_err(|e| VectorError::Internal(e.to_string()))?;
                let rows = stmt
                    .query_map(params![vec_bytes, candidate_limit as i64], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)? as f32))
                    })
                    .map_err(|e| VectorError::Internal(e.to_string()))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(|e| VectorError::Internal(e.to_string()))?
            } else {
                Vec::new()
            };

        // --- Keyword rankings ---
        let kw_ranking: Vec<(String, f32)> =
            if matches!(mode, SearchMode::Keyword | SearchMode::Hybrid) {
                let q = keyword_query.as_deref().unwrap();
                let mut stmt = conn
                    .prepare(&schema.build_fts_bm25_select())
                    .map_err(|e| VectorError::Internal(e.to_string()))?;
                let rows = stmt
                    .query_map(params![q, candidate_limit as i64], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)? as f32))
                    })
                    .map_err(|e| VectorError::Internal(e.to_string()))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(|e| VectorError::Internal(e.to_string()))?
            } else {
                Vec::new()
            };

        // rrf::fuse already truncates to top_k, so no extra slicing needed
        let ids_top: Vec<String> = match mode {
            SearchMode::Vector => vec_ranking.iter().map(|(id, _)| id.clone()).collect(),
            SearchMode::Keyword => kw_ranking.iter().map(|(id, _)| id.clone()).collect(),
            SearchMode::Hybrid => {
                let vec_ids: Vec<String> = vec_ranking.iter().map(|(id, _)| id.clone()).collect();
                let kw_ids: Vec<String> = kw_ranking.iter().map(|(id, _)| id.clone()).collect();
                rrf::fuse(&[vec_ids, kw_ids], top_k, rrf::DEFAULT_RRF_K)
            }
        };

        if ids_top.is_empty() {
            return Ok(Vec::new());
        }

        // Metadata lookup
        let mut stmt = conn
            .prepare(&schema.build_select_metadata_in(ids_top.len()))
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(ids_top.iter()), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map_err(|e| VectorError::Internal(e.to_string()))?;

        let meta_map: std::collections::HashMap<String, serde_json::Value> = rows
            .filter_map(|r| r.ok())
            .map(|(id, meta)| {
                let v = meta
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .unwrap_or(serde_json::Value::Null);
                (id, v)
            })
            .collect();

        let scores: std::collections::HashMap<String, f32> = match mode {
            SearchMode::Vector => vec_ranking.into_iter().collect(),
            SearchMode::Keyword => kw_ranking.into_iter().collect(),
            SearchMode::Hybrid => ids_top
                .iter()
                .enumerate()
                .map(|(i, id)| (id.clone(), 1.0 / (i as f32 + 1.0)))
                .collect(),
        };

        let out: Vec<VectorMatch> = ids_top
            .into_iter()
            .filter_map(|id| {
                let metadata = meta_map.get(&id).cloned();
                if let Some(flt) = filter.as_ref() {
                    if !apply_filter(&metadata, flt) {
                        return None;
                    }
                }
                Some(VectorMatch {
                    id: id.clone(),
                    score: scores.get(&id).copied().unwrap_or(0.0),
                    metadata,
                })
            })
            .take(top_k)
            .collect();

        Ok(out)
    }

    async fn delete(&self, index: &str, ids: Vec<String>) -> Result<(), VectorError> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.db.lock();
        ensure_vec_loaded(&conn).map_err(|e| VectorError::Internal(e.to_string()))?;
        let schema = VectorIndexSchema::new(index);
        if !Self::index_exists(&conn, &schema)? {
            return Err(VectorError::IndexNotFound(index.to_string()));
        }
        let has_kw = Self::has_keyword_search(&conn, &schema)?;

        let tx = conn
            .transaction()
            .map_err(|e| VectorError::Internal(e.to_string()))?;

        // Gather rowids first so we can delete from _vec by rowid.
        let mut stmt = tx
            .prepare(&schema.build_select_rowid_in(ids.len()))
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        let rowids: Vec<i64> = stmt
            .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                r.get::<_, i64>(0)
            })
            .map_err(|e| VectorError::Internal(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);

        let delete_vec_sql = schema.build_delete_vec_by_rowid();
        for rid in rowids {
            tx.execute(&delete_vec_sql, params![rid])
                .map_err(|e| VectorError::Internal(e.to_string()))?;
        }
        tx.execute(
            &schema.build_delete_meta_in(ids.len()),
            rusqlite::params_from_iter(ids.iter()),
        )
        .map_err(|e| VectorError::Internal(e.to_string()))?;
        if has_kw {
            tx.execute(
                &schema.build_delete_fts_in(ids.len()),
                rusqlite::params_from_iter(ids.iter()),
            )
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        }
        tx.commit()
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn count(&self, index: &str) -> Result<u64, VectorError> {
        let conn = self.db.lock();
        ensure_vec_loaded(&conn).map_err(|e| VectorError::Internal(e.to_string()))?;
        let schema = VectorIndexSchema::new(index);
        if !Self::index_exists(&conn, &schema)? {
            return Err(VectorError::IndexNotFound(index.to_string()));
        }
        let n: i64 = conn
            .query_row(&schema.build_count_meta(), [], |r| r.get(0))
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        Ok(n as u64)
    }
}

fn apply_filter(metadata: &Option<serde_json::Value>, filter: &MetadataFilter) -> bool {
    let Some(meta) = metadata else {
        return filter.equals.is_empty();
    };
    for (path, want) in &filter.equals {
        let mut cursor = meta;
        for segment in path.split('.') {
            match cursor.get(segment) {
                Some(v) => cursor = v,
                None => return false,
            }
        }
        if cursor != want {
            return false;
        }
    }
    true
}

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

    #[tokio::test]
    async fn create_index_vector_only() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(cfg("docs", false)).await.unwrap();
        let conn = svc.db.lock();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('docs_vec','docs_meta')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
        let fts_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='docs_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            fts_exists, 0,
            "FTS table must NOT exist when keyword_search=false"
        );
    }

    #[tokio::test]
    async fn create_index_with_keyword_search() {
        let svc = SqliteVecService::open_in_memory().unwrap();
        svc.create_index(cfg("docs", true)).await.unwrap();
        let conn = svc.db.lock();
        let fts_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='docs_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
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
        let conn = svc.db.lock();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'docs_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
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

        let conn = svc.db.lock();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM docs_meta", [], |r| r.get(0))
            .unwrap();
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
        let conn = svc.db.lock();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM docs_meta", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let n_vec: i64 = conn
            .query_row("SELECT COUNT(*) FROM docs_vec", [], |r| r.get(0))
            .unwrap();
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
}
