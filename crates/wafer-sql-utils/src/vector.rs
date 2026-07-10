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
//! by sea-query, so these builders emit SQL strings directly. Index names
//! are validated by [`VectorIndexSchema::new`] via
//! [`crate::ident::validate_ident`] (fail-closed: non-identifier names
//! are rejected, not stripped), so the `format!()` interpolation in the
//! bodies is injection-safe.
//!
//! Parameter binding (the `?N` placeholders) is the caller's
//! responsibility — the builders only emit the SQL template.

use crate::{ident::validate_ident, SqlBuildError};

/// Pre-computed table names for a vector index. Construct once per
/// operation via [`VectorIndexSchema::new`]; pass `&self` to the
/// builders so the index name doesn't have to be re-validated on every
/// SQL emission.
#[derive(Debug, Clone)]
pub struct VectorIndexSchema {
    /// `{name}_vec` — the vec0 virtual table.
    pub vec_table: String,
    /// `{name}_meta` — the metadata side-table.
    pub meta_table: String,
    /// `{name}_fts` — the FTS5 virtual table (only present if
    /// keyword search is enabled; the name is always pre-computed so
    /// callers can use it in `sqlite_master` existence probes without
    /// recomputing).
    pub fts_table: String,
}

impl VectorIndexSchema {
    /// Compute the three table names for an index.
    ///
    /// `name` is interpolated into SQL identifier positions, so it must be a
    /// plain identifier (`[A-Za-z0-9_]`, non-empty). Anything else is
    /// rejected with [`SqlBuildError::InvalidIdentifier`] — fail-closed
    /// rather than silently renaming the index by stripping characters.
    pub fn new(name: &str) -> Result<Self, SqlBuildError> {
        let ident = validate_ident(name)?;
        Ok(Self {
            vec_table: format!("{ident}_vec"),
            meta_table: format!("{ident}_meta"),
            fts_table: format!("{ident}_fts"),
        })
    }

    // ---------- Private helpers ----------

    /// Wrap a DDL SQL string into a [`Statement`] keyed to this index's meta-table.
    fn ddl_stmt(&self, sql: String) -> crate::Statement {
        crate::Statement::new(sql, vec![], self.meta_table.clone())
    }

    // ---------- DDL ----------

    /// `CREATE VIRTUAL TABLE {vec_table} USING vec0(embedding float[{dims}]);`
    /// followed by a regular meta-table create.
    pub fn build_create_vec_and_meta(&self, dims: u32) -> crate::Statement {
        let Self {
            vec_table,
            meta_table,
            ..
        } = self;
        let sql = format!(
            "CREATE VIRTUAL TABLE {vec_table} USING vec0(embedding float[{dims}]);\n\
             CREATE TABLE {meta_table}(\n\
                id TEXT PRIMARY KEY,\n\
                rowid INTEGER NOT NULL,\n\
                metadata TEXT,\n\
                text TEXT\n\
             );"
        );
        self.ddl_stmt(sql)
    }

    /// `CREATE VIRTUAL TABLE {fts_table} USING fts5(id UNINDEXED, text);`
    pub fn build_create_fts(&self) -> crate::Statement {
        let fts_table = &self.fts_table;
        let sql = format!("CREATE VIRTUAL TABLE {fts_table} USING fts5(id UNINDEXED, text);");
        self.ddl_stmt(sql)
    }

    /// Three `DROP TABLE IF EXISTS` statements (vec / meta / fts), in
    /// the order callers should execute them. Each is returned
    /// separately so callers can run them with `Connection::execute`
    /// (which requires a single statement) rather than `execute_batch`.
    pub fn build_drop_all(&self) -> [crate::Statement; 3] {
        [
            crate::Statement::new(
                format!("DROP TABLE IF EXISTS {};", self.vec_table),
                vec![],
                self.meta_table.clone(),
            ),
            crate::Statement::new(
                format!("DROP TABLE IF EXISTS {};", self.fts_table),
                vec![],
                self.meta_table.clone(),
            ),
            crate::Statement::new(
                format!("DROP TABLE IF EXISTS {};", self.meta_table),
                vec![],
                self.meta_table.clone(),
            ),
        ]
    }

    // ---------- Meta-table CRUD ----------

    /// `SELECT rowid FROM {meta_table} WHERE id = ?1`
    pub fn build_select_rowid_by_id(&self) -> crate::Statement {
        let sql = format!("SELECT rowid FROM {} WHERE id = ?1", self.meta_table);
        self.ddl_stmt(sql)
    }

    /// `INSERT INTO {meta_table}(id, rowid, metadata, text)
    /// VALUES (?1, (SELECT COALESCE(MAX(rowid), 0) + 1 FROM {meta_table}), ?2, ?3)`.
    ///
    /// The autoinc-via-subquery shape is SQLite-only (a Postgres port
    /// would use a sequence). Documented here so the SQLite-ism stays
    /// visible.
    pub fn build_insert_meta_autoinc(&self) -> crate::Statement {
        let meta_table = &self.meta_table;
        let sql = format!(
            "INSERT INTO {meta_table}(id, rowid, metadata, text) \
             VALUES (?1, (SELECT COALESCE(MAX(rowid), 0) + 1 FROM {meta_table}), ?2, ?3)"
        );
        self.ddl_stmt(sql)
    }

    /// `UPDATE {meta_table} SET metadata = ?1, text = ?2 WHERE id = ?3`
    pub fn build_update_meta(&self) -> crate::Statement {
        let sql = format!(
            "UPDATE {} SET metadata = ?1, text = ?2 WHERE id = ?3",
            self.meta_table
        );
        self.ddl_stmt(sql)
    }

    /// `SELECT COUNT(*) FROM {meta_table}`
    pub fn build_count_meta(&self) -> crate::Statement {
        let sql = format!("SELECT COUNT(*) FROM {}", self.meta_table);
        self.ddl_stmt(sql)
    }

    /// `SELECT id, metadata FROM {meta_table} WHERE id IN (?,?,...)`
    /// with `n_ids` placeholders. The caller is expected to bind each
    /// id via `params_from_iter`.
    pub fn build_select_metadata_in(&self, n_ids: usize) -> crate::Statement {
        let in_clause = in_clause_placeholders(n_ids);
        let sql = format!(
            "SELECT id, metadata FROM {} WHERE id IN ({in_clause})",
            self.meta_table
        );
        self.ddl_stmt(sql)
    }

    /// `SELECT id FROM {meta_table} WHERE json_extract(metadata, ?) = ? AND …`
    /// with one `(path, value)` placeholder pair per condition. Both the JSON
    /// path (a `$.dotted.path` string) and the value are bound parameters —
    /// nothing caller-supplied is interpolated into the SQL text.
    ///
    /// `n_conditions` must be >= 1 (an unconditioned id dump is not a
    /// supported query shape); panics otherwise — callers validate the
    /// filter before building, this is a programmer-error guard.
    pub fn build_select_ids_by_metadata(&self, n_conditions: usize) -> crate::Statement {
        assert!(
            n_conditions > 0,
            "build_select_ids_by_metadata requires at least one metadata condition"
        );
        let mut clauses = String::new();
        for i in 0..n_conditions {
            if i > 0 {
                clauses.push_str(" AND ");
            }
            let path_param = i * 2 + 1;
            let value_param = i * 2 + 2;
            clauses.push_str(&format!(
                "json_extract(metadata, ?{path_param}) = ?{value_param}"
            ));
        }
        let sql = format!("SELECT id FROM {} WHERE {clauses}", self.meta_table);
        self.ddl_stmt(sql)
    }

    /// `SELECT rowid FROM {meta_table} WHERE id IN (?,?,...)`
    pub fn build_select_rowid_in(&self, n_ids: usize) -> crate::Statement {
        let in_clause = in_clause_placeholders(n_ids);
        let sql = format!(
            "SELECT rowid FROM {} WHERE id IN ({in_clause})",
            self.meta_table
        );
        self.ddl_stmt(sql)
    }

    /// `DELETE FROM {meta_table} WHERE id IN (?,?,...)`
    pub fn build_delete_meta_in(&self, n_ids: usize) -> crate::Statement {
        let in_clause = in_clause_placeholders(n_ids);
        let sql = format!("DELETE FROM {} WHERE id IN ({in_clause})", self.meta_table);
        self.ddl_stmt(sql)
    }

    // ---------- Vec table CRUD ----------

    /// `INSERT INTO {vec_table}(rowid, embedding) VALUES (?1, ?2)`
    pub fn build_insert_vec(&self) -> crate::Statement {
        let sql = format!(
            "INSERT INTO {}(rowid, embedding) VALUES (?1, ?2)",
            self.vec_table
        );
        self.ddl_stmt(sql)
    }

    /// `DELETE FROM {vec_table} WHERE rowid = ?1`
    pub fn build_delete_vec_by_rowid(&self) -> crate::Statement {
        let sql = format!("DELETE FROM {} WHERE rowid = ?1", self.vec_table);
        self.ddl_stmt(sql)
    }

    // ---------- FTS table CRUD ----------

    /// `INSERT INTO {fts_table}(id, text) VALUES (?1, ?2)`
    pub fn build_insert_fts(&self) -> crate::Statement {
        let sql = format!("INSERT INTO {}(id, text) VALUES (?1, ?2)", self.fts_table);
        self.ddl_stmt(sql)
    }

    /// `DELETE FROM {fts_table} WHERE id = ?1`
    pub fn build_delete_fts_by_id(&self) -> crate::Statement {
        let sql = format!("DELETE FROM {} WHERE id = ?1", self.fts_table);
        self.ddl_stmt(sql)
    }

    /// `DELETE FROM {fts_table} WHERE id IN (?,?,...)`
    pub fn build_delete_fts_in(&self, n_ids: usize) -> crate::Statement {
        let in_clause = in_clause_placeholders(n_ids);
        let sql = format!("DELETE FROM {} WHERE id IN ({in_clause})", self.fts_table);
        self.ddl_stmt(sql)
    }

    // ---------- Search queries (SQLite-vec / FTS5 specific) ----------

    /// vec0 KNN by Euclidean distance, joined against the meta table to
    /// project the user id. Bind `?1` to the LE-bytes-encoded query
    /// embedding and `?2` to the candidate `LIMIT`. Result columns:
    /// `(id TEXT, distance REAL)`.
    pub fn build_vec_knn_select(&self) -> crate::Statement {
        let Self {
            vec_table,
            meta_table,
            ..
        } = self;
        let sql = format!(
            "SELECT m.id, v.distance FROM (\
                 SELECT rowid, distance FROM {vec_table} \
                 WHERE embedding MATCH ?1 ORDER BY distance LIMIT ?2\
             ) v JOIN {meta_table} m ON m.rowid = v.rowid \
             ORDER BY v.distance"
        );
        self.ddl_stmt(sql)
    }

    /// FTS5 keyword-rank query using bm25. Bind `?1` to the FTS5 query
    /// string and `?2` to the LIMIT. Result columns: `(id TEXT, score
    /// REAL)`.
    pub fn build_fts_bm25_select(&self) -> crate::Statement {
        let fts_table = &self.fts_table;
        let sql = format!(
            "SELECT id, bm25({fts_table}) AS score \
             FROM {fts_table} WHERE {fts_table} MATCH ?1 \
             ORDER BY score LIMIT ?2"
        );
        self.ddl_stmt(sql)
    }
}

/// Escape LIKE-pattern metacharacters (`\`, `%`, `_`) with a backslash so
/// the input matches literally under `LIKE … ESCAPE '\'`.
pub fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `(sql, bind_pattern)` to list the `_meta` tables of every vector index
/// under `prefix` in `sqlite_master`.
///
/// The prefix is LIKE-escaped so `_`/`%` inside it match literally — unlike
/// `introspect::build_list_tables_like`, whose pattern treats `_` as a
/// single-character wildcard. Bind the returned pattern as `?1`; row column
/// is `name` (the full `{stem}_meta` table name), in lexical order.
pub fn build_list_meta_tables(prefix: &str) -> (String, String) {
    (
        "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE ?1 ESCAPE '\\' ORDER BY name"
            .to_string(),
        format!("{}%\\_meta", escape_like(prefix)),
    )
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
    fn schema_names_derive_from_valid_identifier() {
        let s = VectorIndexSchema::new("docs_v2").expect("plain identifier");
        assert_eq!(s.vec_table, "docs_v2_vec");
        assert_eq!(s.meta_table, "docs_v2_meta");
        assert_eq!(s.fts_table, "docs_v2_fts");
    }

    #[test]
    fn schema_rejects_non_identifier_names() {
        // Fail-closed: previously this was silently stripped to
        // `docsDROPTABLE_*`; now it is rejected so a hostile or mistyped
        // index name can't be laundered into a different valid one.
        let err = VectorIndexSchema::new("docs; DROP TABLE")
            .expect_err("non-identifier index name must be rejected");
        assert!(matches!(
            err,
            crate::SqlBuildError::InvalidIdentifier { .. }
        ));
        assert!(VectorIndexSchema::new("").is_err());
    }

    #[test]
    fn in_clause_placeholders_shapes() {
        assert_eq!(in_clause_placeholders(0), "");
        assert_eq!(in_clause_placeholders(1), "?");
        assert_eq!(in_clause_placeholders(3), "?,?,?");
    }

    #[test]
    fn create_vec_and_meta_uses_table_names() {
        let s = VectorIndexSchema::new("docs").expect("plain identifier");
        let stmt = s.build_create_vec_and_meta(384);
        let sql = stmt.sql;
        assert!(sql.contains("CREATE VIRTUAL TABLE docs_vec USING vec0(embedding float[384])"));
        assert!(sql.contains("CREATE TABLE docs_meta("));
        assert_eq!(stmt.collection, "docs_meta");
    }

    #[test]
    fn drop_all_returns_three_statements_in_order() {
        let s = VectorIndexSchema::new("docs").expect("plain identifier");
        let drops = s.build_drop_all();
        assert_eq!(drops[0].sql, "DROP TABLE IF EXISTS docs_vec;");
        assert_eq!(drops[1].sql, "DROP TABLE IF EXISTS docs_fts;");
        assert_eq!(drops[2].sql, "DROP TABLE IF EXISTS docs_meta;");
        for d in &drops {
            assert_eq!(d.collection, "docs_meta");
        }
    }

    #[test]
    fn in_clause_select_metadata() {
        let s = VectorIndexSchema::new("docs").expect("plain identifier");
        let stmt = s.build_select_metadata_in(3);
        assert_eq!(
            stmt.sql,
            "SELECT id, metadata FROM docs_meta WHERE id IN (?,?,?)"
        );
        assert_eq!(stmt.collection, "docs_meta");
    }

    #[test]
    fn vec_knn_select_uses_both_tables() {
        let s = VectorIndexSchema::new("docs").expect("plain identifier");
        let stmt = s.build_vec_knn_select();
        let sql = stmt.sql;
        assert!(sql.contains("FROM docs_vec"));
        assert!(sql.contains("JOIN docs_meta m"));
        assert!(sql.contains("MATCH ?1"));
        assert!(sql.contains("LIMIT ?2"));
        assert_eq!(stmt.collection, "docs_meta");
    }

    #[test]
    fn fts_bm25_select_uses_fts_table() {
        let s = VectorIndexSchema::new("docs").expect("plain identifier");
        let stmt = s.build_fts_bm25_select();
        let sql = stmt.sql;
        assert!(sql.contains("bm25(docs_fts)"));
        assert!(sql.contains("FROM docs_fts WHERE docs_fts MATCH ?1"));
        assert_eq!(stmt.collection, "docs_meta");
    }

    #[test]
    fn escape_like_escapes_wildcards_and_backslash() {
        assert_eq!(escape_like("a_b%c\\d"), "a\\_b\\%c\\\\d");
        assert_eq!(escape_like("plain"), "plain");
    }

    #[test]
    fn list_meta_tables_pattern_treats_prefix_literally() {
        let (sql, pattern) = build_list_meta_tables("suppers_ai__vector__");
        assert_eq!(
            sql,
            "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE ?1 ESCAPE '\\' ORDER BY name"
        );
        assert_eq!(pattern, "suppers\\_ai\\_\\_vector\\_\\_%\\_meta");
    }

    #[test]
    fn select_ids_by_metadata_binds_paths_and_values() {
        let s = VectorIndexSchema::new("idx").expect("plain identifier");
        let stmt = s.build_select_ids_by_metadata(2);
        assert_eq!(
            stmt.sql,
            "SELECT id FROM idx_meta WHERE json_extract(metadata, ?1) = ?2 AND json_extract(metadata, ?3) = ?4"
        );
        assert_eq!(stmt.collection, "idx_meta");
    }

    #[test]
    #[should_panic(expected = "at least one metadata condition")]
    fn select_ids_by_metadata_rejects_zero_conditions() {
        let s = VectorIndexSchema::new("idx").expect("plain identifier");
        let _ = s.build_select_ids_by_metadata(0);
    }
}
