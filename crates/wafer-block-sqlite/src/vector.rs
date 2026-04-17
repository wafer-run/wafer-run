//! SQLite-backed VectorService using sqlite-vec for similarity search
//! and FTS5 for optional keyword search.

use std::sync::Mutex;

use rusqlite::{params, Connection};
use wafer_core::interfaces::vector::service::{
    DistanceMetric, MetadataFilter, SearchMode, VectorEntry, VectorError, VectorIndexConfig,
    VectorMatch, VectorService,
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

    // consumed in Task 10 upsert
    #[allow(dead_code)]
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

    async fn upsert(&self, _index: &str, _entries: Vec<VectorEntry>) -> Result<(), VectorError> {
        Err(VectorError::Internal("not implemented yet".into()))
    }

    async fn query(
        &self,
        _index: &str,
        _vector: Vec<f32>,
        _top_k: usize,
        _filter: Option<MetadataFilter>,
        _mode: SearchMode,
        _keyword_query: Option<String>,
    ) -> Result<Vec<VectorMatch>, VectorError> {
        Err(VectorError::Internal("not implemented yet".into()))
    }

    async fn delete(&self, _index: &str, _ids: Vec<String>) -> Result<(), VectorError> {
        Err(VectorError::Internal("not implemented yet".into()))
    }

    async fn count(&self, _index: &str) -> Result<u64, VectorError> {
        Err(VectorError::Internal("not implemented yet".into()))
    }
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
}
