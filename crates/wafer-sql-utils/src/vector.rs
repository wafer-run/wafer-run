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

use wafer_block::wire::vector::is_legacy_spelling_of;

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
    /// plain identifier ([`validate_ident`]: 1 to 63 of lowercase ASCII
    /// letters, digits and `_`). Anything else is rejected with
    /// [`SqlBuildError::InvalidIdentifier`] — fail-closed rather than
    /// silently renaming the index by stripping characters.
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

    /// [`Self::build_vec_knn_select`] restricted to entries whose metadata
    /// satisfies a filter, so the `LIMIT` counts only matching entries.
    ///
    /// The restriction is a `rowid IN (…)` constraint that vec0 applies
    /// inside the KNN scan; it calls [`METADATA_FILTER_FN`], which the
    /// connection must have registered. Bind `?1` to the LE-bytes query
    /// embedding, `?2` to the `LIMIT`, and `?3` to the filter as JSON.
    /// Result columns: `(id TEXT, distance REAL)`.
    pub fn build_vec_knn_select_filtered(&self) -> crate::Statement {
        let Self {
            vec_table,
            meta_table,
            ..
        } = self;
        let sql = format!(
            "SELECT m.id, v.distance FROM (\
                 SELECT rowid, distance FROM {vec_table} \
                 WHERE embedding MATCH ?1 \
                 AND rowid IN (SELECT rowid FROM {meta_table} \
                     WHERE {METADATA_FILTER_FN}(metadata, ?3)) \
                 ORDER BY distance LIMIT ?2\
             ) v JOIN {meta_table} m ON m.rowid = v.rowid \
             ORDER BY v.distance"
        );
        self.ddl_stmt(sql)
    }

    /// [`Self::build_fts_bm25_select`] restricted to entries whose metadata
    /// satisfies a filter, so the `LIMIT` counts only matching entries.
    /// Calls [`METADATA_FILTER_FN`], which the connection must have
    /// registered. Bind `?1` to the FTS5 query string, `?2` to the `LIMIT`,
    /// and `?3` to the filter as JSON. Result columns: `(id TEXT, score
    /// REAL)`.
    pub fn build_fts_bm25_select_filtered(&self) -> crate::Statement {
        let Self {
            meta_table,
            fts_table,
            ..
        } = self;
        let sql = format!(
            "SELECT id, bm25({fts_table}) AS score \
             FROM {fts_table} WHERE {fts_table} MATCH ?1 \
             AND id IN (SELECT id FROM {meta_table} \
                 WHERE {METADATA_FILTER_FN}(metadata, ?3)) \
             ORDER BY score LIMIT ?2"
        );
        self.ddl_stmt(sql)
    }
}

/// Table names and statements that move a vector index stored under a
/// legacy name (one with uppercase letters, from before index names had to
/// be lowercase) to its lowercase spelling.
///
/// SQLite folds identifier case, so `Docs_meta` cannot be renamed straight
/// to `docs_meta` (SQLite reports the target as already taken — by the table
/// being renamed) and a `docs_vec` vec0 table cannot be created while
/// `Docs_vec` exists. Every table therefore moves through a staging stem,
/// `{to}-rename`, which no index can be stored under: index names never
/// contain `-`. Regular and FTS5 tables are renamed (FTS5 renames its shadow
/// tables with it). A vec0 table cannot be renamed — sqlite-vec implements
/// no `xRename`, so `ALTER TABLE` would leave its shadow tables behind under
/// the old name — so its rows are copied, rowids included, into a new vec0
/// table declared with the old one's module arguments, and the old table is
/// dropped. Copying the rowids keeps every vector aligned with its
/// `{name}_meta` row.
///
/// The statements do not check for existing tables; the caller probes the
/// catalog, and runs them in one transaction.
#[derive(Debug, Clone)]
pub struct VectorIndexRename {
    /// Tables of the index being moved (legacy name).
    pub from: VectorIndexSchema,
    /// Tables of the staging stem the move passes through.
    pub staging: VectorIndexSchema,
    /// Tables of the index once moved.
    pub to: VectorIndexSchema,
}

impl VectorIndexRename {
    /// Compute the table names for moving `from` to `to`. `from` must be a
    /// legacy spelling of `to` ([`is_legacy_spelling_of`]); anything else is
    /// [`SqlBuildError::InvalidIdentifier`] naming the rejected name. That
    /// rule also keeps `from` to ASCII letters, digits and `_`, so both names
    /// are safe in identifier positions.
    pub fn new(from: &str, to: &str) -> Result<Self, SqlBuildError> {
        let to_schema = VectorIndexSchema::new(to)?;
        if !is_legacy_spelling_of(from, to) {
            return Err(SqlBuildError::InvalidIdentifier {
                value: from.to_string(),
            });
        }
        let names = |stem: &str| VectorIndexSchema {
            vec_table: format!("{stem}_vec"),
            meta_table: format!("{stem}_meta"),
            fts_table: format!("{stem}_fts"),
        };
        Ok(Self {
            from: names(from),
            staging: names(&format!("{to}-rename")),
            to: to_schema,
        })
    }

    /// The statements that move the index, in execution order. Each holds
    /// one SQL statement.
    ///
    /// - `vec_module_args` is the `vec0(…)` clause of the `from` vec table's
    ///   `CREATE VIRTUAL TABLE` text, as [`vec0_module_args`] extracts it from
    ///   `sqlite_master`; the new vec tables are declared with it verbatim.
    /// - `vec_columns` are that table's declared columns (`pragma_table_info`
    ///   order), copied with the rowid.
    /// - `keyword_search` says whether the index has an FTS table to move.
    pub fn build_statements(
        &self,
        vec_module_args: &str,
        vec_columns: &[String],
        keyword_search: bool,
    ) -> Vec<crate::Statement> {
        let Self { from, staging, to } = self;
        let columns: String = vec_columns
            .iter()
            .map(|c| format!(", {}", quote_ident(c)))
            .collect();
        let copy_vec = |src: &str, dst: &str| {
            format!(
                "INSERT INTO {dst}(rowid{columns}) SELECT rowid{columns} FROM {src}",
                src = quote_ident(src),
                dst = quote_ident(dst),
            )
        };
        let create_vec = |table: &str| {
            format!(
                "CREATE VIRTUAL TABLE {} USING {vec_module_args}",
                quote_ident(table)
            )
        };
        let rename = |a: &str, b: &str| {
            format!(
                "ALTER TABLE {} RENAME TO {}",
                quote_ident(a),
                quote_ident(b)
            )
        };
        let drop = |table: &str| format!("DROP TABLE {}", quote_ident(table));

        let mut sql = vec![
            create_vec(&staging.vec_table),
            copy_vec(&from.vec_table, &staging.vec_table),
            drop(&from.vec_table),
            create_vec(&to.vec_table),
            copy_vec(&staging.vec_table, &to.vec_table),
            drop(&staging.vec_table),
            rename(&from.meta_table, &staging.meta_table),
            rename(&staging.meta_table, &to.meta_table),
        ];
        if keyword_search {
            sql.push(rename(&from.fts_table, &staging.fts_table));
            sql.push(rename(&staging.fts_table, &to.fts_table));
        }
        sql.into_iter()
            .map(|sql| crate::Statement::new(sql, vec![], to.meta_table.clone()))
            .collect()
    }
}

/// The module clause (`vec0(embedding float[3])`) of a
/// `CREATE VIRTUAL TABLE … USING …` statement as `sqlite_master.sql` stores
/// it, or `None` when `create_sql` has no `USING` clause.
pub fn vec0_module_args(create_sql: &str) -> Option<&str> {
    let upper = create_sql.to_ascii_uppercase();
    let at = upper.find(" USING ")?;
    Some(create_sql[at + " USING ".len()..].trim())
}

/// `name` as a double-quoted SQL identifier (`"` doubled).
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Name of the SQL scalar function the `*_filtered` search builders call
/// as `METADATA_FILTER_FN(metadata, filter_json)`: true when the entry's
/// `metadata` column satisfies the metadata filter serialized in
/// `filter_json`. The builders only reference it; the connection that runs
/// the statement registers it.
pub const METADATA_FILTER_FN: &str = "wafer_vector_metadata_matches";

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
    fn rename_moves_every_table_through_the_staging_stem() {
        let r = VectorIndexRename::new("Docs", "docs").expect("legacy spelling");
        assert_eq!(r.from.vec_table, "Docs_vec");
        assert_eq!(r.staging.meta_table, "docs-rename_meta");
        assert_eq!(r.to.fts_table, "docs_fts");
        let sql: Vec<String> = r
            .build_statements("vec0(embedding float[3])", &["embedding".into()], true)
            .into_iter()
            .map(|s| s.sql)
            .collect();
        assert_eq!(
            sql,
            vec![
                "CREATE VIRTUAL TABLE \"docs-rename_vec\" USING vec0(embedding float[3])",
                "INSERT INTO \"docs-rename_vec\"(rowid, \"embedding\") SELECT rowid, \"embedding\" FROM \"Docs_vec\"",
                "DROP TABLE \"Docs_vec\"",
                "CREATE VIRTUAL TABLE \"docs_vec\" USING vec0(embedding float[3])",
                "INSERT INTO \"docs_vec\"(rowid, \"embedding\") SELECT rowid, \"embedding\" FROM \"docs-rename_vec\"",
                "DROP TABLE \"docs-rename_vec\"",
                "ALTER TABLE \"Docs_meta\" RENAME TO \"docs-rename_meta\"",
                "ALTER TABLE \"docs-rename_meta\" RENAME TO \"docs_meta\"",
                "ALTER TABLE \"Docs_fts\" RENAME TO \"docs-rename_fts\"",
                "ALTER TABLE \"docs-rename_fts\" RENAME TO \"docs_fts\"",
            ]
        );
        let without_fts = r.build_statements("vec0(embedding float[3])", &[], false);
        assert_eq!(without_fts.len(), 8);
    }

    #[test]
    fn rename_rejects_anything_but_a_legacy_spelling() {
        for (from, to, rejected) in [
            ("docs", "docs", "docs"),
            ("Docs", "other", "Docs"),
            (
                "Docs\"; DROP TABLE x; --",
                "docs\"; drop table x; --",
                "docs\"; drop table x; --",
            ),
            ("Docs", "Docs", "Docs"),
        ] {
            assert_eq!(
                VectorIndexRename::new(from, to).map(|_| ()),
                Err(SqlBuildError::InvalidIdentifier {
                    value: rejected.to_string()
                }),
                "{from:?} -> {to:?}"
            );
        }
    }

    #[test]
    fn vec0_module_args_takes_the_clause_after_using() {
        assert_eq!(
            vec0_module_args("CREATE VIRTUAL TABLE Docs_vec USING vec0(embedding float[1024])"),
            Some("vec0(embedding float[1024])")
        );
        assert_eq!(
            vec0_module_args("create virtual table x using vec0(embedding float[3])"),
            Some("vec0(embedding float[3])")
        );
        assert_eq!(vec0_module_args("CREATE TABLE x(a)"), None);
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
    fn filtered_search_selects_restrict_inside_the_limited_query() {
        let s = VectorIndexSchema::new("docs").expect("plain identifier");
        let knn = s.build_vec_knn_select_filtered().sql;
        assert!(knn.contains(
            "WHERE embedding MATCH ?1 AND rowid IN (SELECT rowid FROM docs_meta \
             WHERE wafer_vector_metadata_matches(metadata, ?3)) ORDER BY distance LIMIT ?2"
        ));
        let fts = s.build_fts_bm25_select_filtered().sql;
        assert!(fts.contains(
            "WHERE docs_fts MATCH ?1 AND id IN (SELECT id FROM docs_meta \
             WHERE wafer_vector_metadata_matches(metadata, ?3)) ORDER BY score LIMIT ?2"
        ));
    }

    #[test]
    fn escape_like_escapes_wildcards_and_backslash() {
        assert_eq!(escape_like("a_b%c\\d"), "a\\_b\\%c\\\\d");
        assert_eq!(escape_like("plain"), "plain");
    }

    #[test]
    fn list_meta_tables_pattern_treats_prefix_literally() {
        let (sql, pattern) = build_list_meta_tables("my_org__vector__");
        assert_eq!(
            sql,
            "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE ?1 ESCAPE '\\' ORDER BY name"
        );
        assert_eq!(pattern, "my\\_org\\_\\_vector\\_\\_%\\_meta");
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
