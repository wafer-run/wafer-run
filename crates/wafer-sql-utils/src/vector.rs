//! SQL builders for vector-store schemas backed by `sqlite-vec` (vec0) and
//! optional FTS5 keyword search.
//!
//! The schema is opinionated: each vector index is materialised as three
//! tables — `{name}_vec` (vec0 virtual table holding embeddings),
//! `{name}_meta` (regular table holding ids, metadata, source text), and
//! optionally `{name}_fts` (FTS5 virtual table for keyword search). The
//! call site picks the metric / dimensions / keyword-search flag; the
//! builders below produce the SQL.
//!
//! The vec0 `MATCH` operator, FTS5 `bm25` function, and `CREATE VIRTUAL
//! TABLE USING vec0/fts5` syntax are SQLite-only and cannot be modelled
//! by sea-query, so these builders emit SQL strings directly. Table
//! names are pre-sanitised via [`crate::ident::sanitize_ident`], so the
//! `format!()` interpolation in the bodies is injection-safe.
//!
//! Parameter binding (the `?N` placeholders) is the caller's
//! responsibility — the builders only emit the SQL template.

use crate::ident::sanitize_ident;

/// Pre-computed table names for a vector index. Construct once per
/// operation via [`VectorIndexSchema::new`]; pass `&self` to the
/// builders so the table names don't have to be re-sanitised on every
/// SQL emission.
#[derive(Debug, Clone)]
pub struct VectorIndexSchema {
    /// `{sanitised_name}_vec` — the vec0 virtual table.
    pub vec_table: String,
    /// `{sanitised_name}_meta` — the metadata side-table.
    pub meta_table: String,
    /// `{sanitised_name}_fts` — the FTS5 virtual table (only present if
    /// keyword search is enabled; the name is always pre-computed so
    /// callers can use it in `sqlite_master` existence probes without
    /// recomputing).
    pub fts_table: String,
}

impl VectorIndexSchema {
    /// Compute the three table names for an index. `name` is sanitised
    /// to alphanumeric + underscore to ensure the result is safe to
    /// interpolate into a SQL identifier position.
    pub fn new(name: &str) -> Self {
        let ident = sanitize_ident(name);
        Self {
            vec_table: format!("{ident}_vec"),
            meta_table: format!("{ident}_meta"),
            fts_table: format!("{ident}_fts"),
        }
    }

    // ---------- DDL ----------

    /// `CREATE VIRTUAL TABLE {vec_table} USING vec0(embedding float[{dims}]);`
    /// followed by a regular meta-table create.
    pub fn build_create_vec_and_meta(&self, dims: u32) -> String {
        let Self {
            vec_table,
            meta_table,
            ..
        } = self;
        format!(
            "CREATE VIRTUAL TABLE {vec_table} USING vec0(embedding float[{dims}]);\n\
             CREATE TABLE {meta_table}(\n\
                id TEXT PRIMARY KEY,\n\
                rowid INTEGER NOT NULL,\n\
                metadata TEXT,\n\
                text TEXT\n\
             );"
        )
    }

    /// `CREATE VIRTUAL TABLE {fts_table} USING fts5(id UNINDEXED, text);`
    pub fn build_create_fts(&self) -> String {
        let fts_table = &self.fts_table;
        format!("CREATE VIRTUAL TABLE {fts_table} USING fts5(id UNINDEXED, text);")
    }

    /// Three `DROP TABLE IF EXISTS` statements (vec / meta / fts), in
    /// the order callers should execute them. Each is returned
    /// separately so callers can run them with `Connection::execute`
    /// (which requires a single statement) rather than `execute_batch`.
    pub fn build_drop_all(&self) -> [String; 3] {
        [
            format!("DROP TABLE IF EXISTS {};", self.vec_table),
            format!("DROP TABLE IF EXISTS {};", self.fts_table),
            format!("DROP TABLE IF EXISTS {};", self.meta_table),
        ]
    }

    // ---------- Meta-table CRUD ----------

    /// `SELECT rowid FROM {meta_table} WHERE id = ?1`
    pub fn build_select_rowid_by_id(&self) -> String {
        format!("SELECT rowid FROM {} WHERE id = ?1", self.meta_table)
    }

    /// `INSERT INTO {meta_table}(id, rowid, metadata, text)
    /// VALUES (?1, (SELECT COALESCE(MAX(rowid), 0) + 1 FROM {meta_table}), ?2, ?3)`.
    ///
    /// The autoinc-via-subquery shape is SQLite-only (a Postgres port
    /// would use a sequence). Documented here so the SQLite-ism stays
    /// visible.
    pub fn build_insert_meta_autoinc(&self) -> String {
        let meta_table = &self.meta_table;
        format!(
            "INSERT INTO {meta_table}(id, rowid, metadata, text) \
             VALUES (?1, (SELECT COALESCE(MAX(rowid), 0) + 1 FROM {meta_table}), ?2, ?3)"
        )
    }

    /// `UPDATE {meta_table} SET metadata = ?1, text = ?2 WHERE id = ?3`
    pub fn build_update_meta(&self) -> String {
        format!(
            "UPDATE {} SET metadata = ?1, text = ?2 WHERE id = ?3",
            self.meta_table
        )
    }

    /// `SELECT COUNT(*) FROM {meta_table}`
    pub fn build_count_meta(&self) -> String {
        format!("SELECT COUNT(*) FROM {}", self.meta_table)
    }

    /// `SELECT id, metadata FROM {meta_table} WHERE id IN (?,?,...)`
    /// with `n_ids` placeholders. The caller is expected to bind each
    /// id via `params_from_iter`.
    pub fn build_select_metadata_in(&self, n_ids: usize) -> String {
        let in_clause = in_clause_placeholders(n_ids);
        format!(
            "SELECT id, metadata FROM {} WHERE id IN ({in_clause})",
            self.meta_table
        )
    }

    /// `SELECT rowid FROM {meta_table} WHERE id IN (?,?,...)`
    pub fn build_select_rowid_in(&self, n_ids: usize) -> String {
        let in_clause = in_clause_placeholders(n_ids);
        format!(
            "SELECT rowid FROM {} WHERE id IN ({in_clause})",
            self.meta_table
        )
    }

    /// `DELETE FROM {meta_table} WHERE id IN (?,?,...)`
    pub fn build_delete_meta_in(&self, n_ids: usize) -> String {
        let in_clause = in_clause_placeholders(n_ids);
        format!("DELETE FROM {} WHERE id IN ({in_clause})", self.meta_table)
    }

    // ---------- Vec table CRUD ----------

    /// `INSERT INTO {vec_table}(rowid, embedding) VALUES (?1, ?2)`
    pub fn build_insert_vec(&self) -> String {
        format!(
            "INSERT INTO {}(rowid, embedding) VALUES (?1, ?2)",
            self.vec_table
        )
    }

    /// `DELETE FROM {vec_table} WHERE rowid = ?1`
    pub fn build_delete_vec_by_rowid(&self) -> String {
        format!("DELETE FROM {} WHERE rowid = ?1", self.vec_table)
    }

    // ---------- FTS table CRUD ----------

    /// `INSERT INTO {fts_table}(id, text) VALUES (?1, ?2)`
    pub fn build_insert_fts(&self) -> String {
        format!("INSERT INTO {}(id, text) VALUES (?1, ?2)", self.fts_table)
    }

    /// `DELETE FROM {fts_table} WHERE id = ?1`
    pub fn build_delete_fts_by_id(&self) -> String {
        format!("DELETE FROM {} WHERE id = ?1", self.fts_table)
    }

    /// `DELETE FROM {fts_table} WHERE id IN (?,?,...)`
    pub fn build_delete_fts_in(&self, n_ids: usize) -> String {
        let in_clause = in_clause_placeholders(n_ids);
        format!("DELETE FROM {} WHERE id IN ({in_clause})", self.fts_table)
    }

    // ---------- Search queries (SQLite-vec / FTS5 specific) ----------

    /// vec0 KNN by Euclidean distance, joined against the meta table to
    /// project the user id. Bind `?1` to the LE-bytes-encoded query
    /// embedding and `?2` to the candidate `LIMIT`. Result columns:
    /// `(id TEXT, distance REAL)`.
    pub fn build_vec_knn_select(&self) -> String {
        let Self {
            vec_table,
            meta_table,
            ..
        } = self;
        format!(
            "SELECT m.id, v.distance FROM (\
                 SELECT rowid, distance FROM {vec_table} \
                 WHERE embedding MATCH ?1 ORDER BY distance LIMIT ?2\
             ) v JOIN {meta_table} m ON m.rowid = v.rowid \
             ORDER BY v.distance"
        )
    }

    /// FTS5 keyword-rank query using bm25. Bind `?1` to the FTS5 query
    /// string and `?2` to the LIMIT. Result columns: `(id TEXT, score
    /// REAL)`.
    pub fn build_fts_bm25_select(&self) -> String {
        let fts_table = &self.fts_table;
        format!(
            "SELECT id, bm25({fts_table}) AS score \
             FROM {fts_table} WHERE {fts_table} MATCH ?1 \
             ORDER BY score LIMIT ?2"
        )
    }
}

/// `?,?,?...` with `n` placeholders. Returns an empty string when
/// `n == 0`; callers should guard against empty input themselves.
fn in_clause_placeholders(n: usize) -> String {
    let mut s = String::with_capacity(n * 2);
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        s.push('?');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_names_are_sanitised() {
        let s = VectorIndexSchema::new("docs; DROP TABLE");
        assert_eq!(s.vec_table, "docsDROPTABLE_vec");
        assert_eq!(s.meta_table, "docsDROPTABLE_meta");
        assert_eq!(s.fts_table, "docsDROPTABLE_fts");
    }

    #[test]
    fn in_clause_placeholders_shapes() {
        assert_eq!(in_clause_placeholders(0), "");
        assert_eq!(in_clause_placeholders(1), "?");
        assert_eq!(in_clause_placeholders(3), "?,?,?");
    }

    #[test]
    fn create_vec_and_meta_uses_table_names() {
        let s = VectorIndexSchema::new("docs");
        let sql = s.build_create_vec_and_meta(384);
        assert!(sql.contains("CREATE VIRTUAL TABLE docs_vec USING vec0(embedding float[384])"));
        assert!(sql.contains("CREATE TABLE docs_meta("));
    }

    #[test]
    fn drop_all_returns_three_statements_in_order() {
        let s = VectorIndexSchema::new("docs");
        let drops = s.build_drop_all();
        assert_eq!(drops[0], "DROP TABLE IF EXISTS docs_vec;");
        assert_eq!(drops[1], "DROP TABLE IF EXISTS docs_fts;");
        assert_eq!(drops[2], "DROP TABLE IF EXISTS docs_meta;");
    }

    #[test]
    fn in_clause_select_metadata() {
        let s = VectorIndexSchema::new("docs");
        assert_eq!(
            s.build_select_metadata_in(3),
            "SELECT id, metadata FROM docs_meta WHERE id IN (?,?,?)"
        );
    }

    #[test]
    fn vec_knn_select_uses_both_tables() {
        let s = VectorIndexSchema::new("docs");
        let sql = s.build_vec_knn_select();
        assert!(sql.contains("FROM docs_vec"));
        assert!(sql.contains("JOIN docs_meta m"));
        assert!(sql.contains("MATCH ?1"));
        assert!(sql.contains("LIMIT ?2"));
    }

    #[test]
    fn fts_bm25_select_uses_fts_table() {
        let s = VectorIndexSchema::new("docs");
        let sql = s.build_fts_bm25_select();
        assert!(sql.contains("bm25(docs_fts)"));
        assert!(sql.contains("FROM docs_fts WHERE docs_fts MATCH ?1"));
    }
}
