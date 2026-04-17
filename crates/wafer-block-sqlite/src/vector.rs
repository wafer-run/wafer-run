//! SQLite-backed VectorService using sqlite-vec for similarity search
//! and FTS5 for optional keyword search.

use std::sync::Mutex;

use rusqlite::{params, Connection};
use wafer_core::interfaces::vector::{
    rrf,
    service::{
        DistanceMetric, MetadataFilter, SearchMode, VectorEntry, VectorError, VectorIndexConfig,
        VectorMatch, VectorService,
    },
};
use wafer_sql_utils::ident::sanitize_ident;

use crate::ensure_vec_loaded;

pub struct SqliteVecService {
    db: Mutex<Connection>,
}

impl SqliteVecService {
    pub fn new(db: Connection) -> Self {
        Self { db: Mutex::new(db) }
    }

    pub fn open_in_memory() -> rusqlite::Result<Self> {
        // Register the sqlite-vec auto-extension BEFORE opening the connection.
        // `sqlite3_auto_extension` only affects connections opened after
        // registration, so a conn opened first will not have vec0 available.
        let probe = Connection::open_in_memory()?;
        ensure_vec_loaded(&probe)?;
        drop(probe);
        Ok(Self::new(Connection::open_in_memory()?))
    }

    fn table_name(index: &str, suffix: &str) -> String {
        let ident = sanitize_ident(index);
        format!("{ident}_{suffix}")
    }

    fn index_exists(conn: &Connection, index: &str) -> Result<bool, VectorError> {
        let vec_tbl = Self::table_name(index, "vec");
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                params![vec_tbl],
                |row| row.get(0),
            )
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        Ok(exists)
    }

    fn has_keyword_search(conn: &Connection, index: &str) -> Result<bool, VectorError> {
        let fts_tbl = Self::table_name(index, "fts");
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                params![fts_tbl],
                |row| row.get(0),
            )
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        Ok(exists)
    }
}

#[async_trait::async_trait]
impl VectorService for SqliteVecService {
    async fn create_index(&self, config: VectorIndexConfig) -> Result<(), VectorError> {
        let conn = self.db.lock().unwrap();
        ensure_vec_loaded(&conn).map_err(|e| VectorError::Internal(e.to_string()))?;
        if Self::index_exists(&conn, &config.name)? {
            return Err(VectorError::IndexAlreadyExists(config.name));
        }
        let vec_tbl = Self::table_name(&config.name, "vec");
        let meta_tbl = Self::table_name(&config.name, "meta");
        let dims = config.dimensions;
        // All 3 metric variants are accepted — sqlite-vec is distance-agnostic at storage;
        // it operates as cosine distance in SQL queries regardless of the stored metric tag.
        let _ = match config.metric {
            DistanceMetric::Cosine | DistanceMetric::Euclidean | DistanceMetric::DotProduct => (),
        };
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE {vec_tbl} USING vec0(embedding float[{dims}]);
             CREATE TABLE {meta_tbl}(
                id TEXT PRIMARY KEY,
                rowid INTEGER NOT NULL,
                metadata TEXT,
                text TEXT
             );"
        ))
        .map_err(|e| VectorError::Internal(e.to_string()))?;
        if config.keyword_search {
            let fts_tbl = Self::table_name(&config.name, "fts");
            conn.execute_batch(&format!(
                "CREATE VIRTUAL TABLE {fts_tbl} USING fts5(id UNINDEXED, text);"
            ))
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        }
        Ok(())
    }

    async fn delete_index(&self, name: &str) -> Result<(), VectorError> {
        let conn = self.db.lock().unwrap();
        ensure_vec_loaded(&conn).map_err(|e| VectorError::Internal(e.to_string()))?;
        if !Self::index_exists(&conn, name)? {
            return Err(VectorError::IndexNotFound(name.to_string()));
        }
        for suffix in ["vec", "fts", "meta"] {
            let tbl = Self::table_name(name, suffix);
            conn.execute(&format!("DROP TABLE IF EXISTS {tbl};"), [])
                .map_err(|e| VectorError::Internal(e.to_string()))?;
        }
        Ok(())
    }

    async fn upsert(&self, index: &str, entries: Vec<VectorEntry>) -> Result<(), VectorError> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut conn = self.db.lock().unwrap();
        ensure_vec_loaded(&conn).map_err(|e| VectorError::Internal(e.to_string()))?;
        if !Self::index_exists(&conn, index)? {
            return Err(VectorError::IndexNotFound(index.to_string()));
        }
        let has_kw = Self::has_keyword_search(&conn, index)?;
        for e in &entries {
            if has_kw && e.text.is_none() {
                return Err(VectorError::TextRequired);
            }
        }
        let vec_tbl = Self::table_name(index, "vec");
        let meta_tbl = Self::table_name(index, "meta");
        let fts_tbl = Self::table_name(index, "fts");

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
                .query_row(
                    &format!("SELECT rowid FROM {meta_tbl} WHERE id = ?1"),
                    params![&e.id],
                    |r| r.get(0),
                )
                .ok();
            let rowid = match rowid {
                Some(rid) => {
                    tx.execute(
                        &format!("DELETE FROM {vec_tbl} WHERE rowid = ?1"),
                        params![rid],
                    )
                    .map_err(|err| VectorError::Internal(err.to_string()))?;
                    rid
                }
                None => {
                    tx.execute(
                        &format!(
                            "INSERT INTO {meta_tbl}(id, rowid, metadata, text) \
                             VALUES (?1, (SELECT COALESCE(MAX(rowid), 0) + 1 FROM {meta_tbl}), ?2, ?3)"
                        ),
                        params![&e.id, meta_json, e.text.clone().unwrap_or_default()],
                    )
                    .map_err(|err| VectorError::Internal(err.to_string()))?;
                    tx.query_row(
                        &format!("SELECT rowid FROM {meta_tbl} WHERE id = ?1"),
                        params![&e.id],
                        |r| r.get::<_, i64>(0),
                    )
                    .map_err(|err| VectorError::Internal(err.to_string()))?
                }
            };

            let vec_bytes: Vec<u8> = e.vector.iter().flat_map(|f| f.to_le_bytes()).collect();
            tx.execute(
                &format!("INSERT INTO {vec_tbl}(rowid, embedding) VALUES (?1, ?2)"),
                params![rowid, vec_bytes],
            )
            .map_err(|err| VectorError::Internal(err.to_string()))?;

            // Update meta (metadata + text may have changed on re-upsert)
            tx.execute(
                &format!("UPDATE {meta_tbl} SET metadata = ?1, text = ?2 WHERE id = ?3"),
                params![meta_json, e.text.clone().unwrap_or_default(), &e.id],
            )
            .map_err(|err| VectorError::Internal(err.to_string()))?;

            if has_kw {
                let text = e.text.unwrap_or_default();
                tx.execute(
                    &format!("DELETE FROM {fts_tbl} WHERE id = ?1"),
                    params![&e.id],
                )
                .map_err(|err| VectorError::Internal(err.to_string()))?;
                tx.execute(
                    &format!("INSERT INTO {fts_tbl}(id, text) VALUES (?1, ?2)"),
                    params![&e.id, text],
                )
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
        let conn = self.db.lock().unwrap();
        ensure_vec_loaded(&conn).map_err(|e| VectorError::Internal(e.to_string()))?;
        if !Self::index_exists(&conn, index)? {
            return Err(VectorError::IndexNotFound(index.to_string()));
        }
        let has_kw = Self::has_keyword_search(&conn, index)?;
        match mode {
            SearchMode::Keyword | SearchMode::Hybrid if !has_kw => {
                return Err(VectorError::KeywordSearchNotEnabled);
            }
            SearchMode::Keyword | SearchMode::Hybrid if keyword_query.is_none() => {
                return Err(VectorError::KeywordQueryRequired(mode));
            }
            _ => {}
        }
        let vec_tbl = Self::table_name(index, "vec");
        let meta_tbl = Self::table_name(index, "meta");
        let fts_tbl = Self::table_name(index, "fts");

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
                    .prepare(&format!(
                        "SELECT m.id, v.distance FROM (\
                             SELECT rowid, distance FROM {vec_tbl} \
                             WHERE embedding MATCH ?1 ORDER BY distance LIMIT ?2\
                         ) v JOIN {meta_tbl} m ON m.rowid = v.rowid \
                         ORDER BY v.distance"
                    ))
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
                    .prepare(&format!(
                        "SELECT id, bm25({fts_tbl}) AS score \
                         FROM {fts_tbl} WHERE {fts_tbl} MATCH ?1 \
                         ORDER BY score LIMIT ?2"
                    ))
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
        let placeholders: Vec<&str> = (0..ids_top.len()).map(|_| "?").collect();
        let in_clause = placeholders.join(",");
        let mut stmt = conn
            .prepare(&format!(
                "SELECT id, metadata FROM {meta_tbl} WHERE id IN ({in_clause})"
            ))
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
        let mut conn = self.db.lock().unwrap();
        ensure_vec_loaded(&conn).map_err(|e| VectorError::Internal(e.to_string()))?;
        if !Self::index_exists(&conn, index)? {
            return Err(VectorError::IndexNotFound(index.to_string()));
        }
        let has_kw = Self::has_keyword_search(&conn, index)?;
        let vec_tbl = Self::table_name(index, "vec");
        let meta_tbl = Self::table_name(index, "meta");
        let fts_tbl = Self::table_name(index, "fts");

        let tx = conn
            .transaction()
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        let placeholders: Vec<&str> = (0..ids.len()).map(|_| "?").collect();
        let in_clause = placeholders.join(",");

        // Gather rowids first so we can delete from _vec by rowid.
        let mut stmt = tx
            .prepare(&format!(
                "SELECT rowid FROM {meta_tbl} WHERE id IN ({in_clause})"
            ))
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        let rowids: Vec<i64> = stmt
            .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                r.get::<_, i64>(0)
            })
            .map_err(|e| VectorError::Internal(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);

        for rid in rowids {
            tx.execute(
                &format!("DELETE FROM {vec_tbl} WHERE rowid = ?1"),
                params![rid],
            )
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        }
        tx.execute(
            &format!("DELETE FROM {meta_tbl} WHERE id IN ({in_clause})"),
            rusqlite::params_from_iter(ids.iter()),
        )
        .map_err(|e| VectorError::Internal(e.to_string()))?;
        if has_kw {
            tx.execute(
                &format!("DELETE FROM {fts_tbl} WHERE id IN ({in_clause})"),
                rusqlite::params_from_iter(ids.iter()),
            )
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        }
        tx.commit()
            .map_err(|e| VectorError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn count(&self, index: &str) -> Result<u64, VectorError> {
        let conn = self.db.lock().unwrap();
        ensure_vec_loaded(&conn).map_err(|e| VectorError::Internal(e.to_string()))?;
        if !Self::index_exists(&conn, index)? {
            return Err(VectorError::IndexNotFound(index.to_string()));
        }
        let meta_tbl = Self::table_name(index, "meta");
        let n: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {meta_tbl}"), [], |r| {
                r.get(0)
            })
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
        let conn = svc.db.lock().unwrap();
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
        let conn = svc.db.lock().unwrap();
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
        let conn = svc.db.lock().unwrap();
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

        let conn = svc.db.lock().unwrap();
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
        let conn = svc.db.lock().unwrap();
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
