//! Roundtrip test for the `wafer-sql-utils::vector` builder migration.
//!
//! Exercises create_index / upsert / query / delete / count against an
//! in-memory SQLite-vec database using hand-crafted f32 vectors, so it
//! runs without the 120 MB FastembedService model download.

#![cfg(feature = "vectors")]

use wafer_block_sqlite::vector::SqliteVecService;
use wafer_core::interfaces::vector::{
    service::{VectorEntry, VectorIndexConfig, VectorService},
    DistanceMetric, SearchMode,
};

fn unit_vec(i: usize) -> Vec<f32> {
    // Hand-crafted distinguishable 4-dim unit-ish vectors.
    let mut v = vec![0.0; 4];
    v[i % 4] = 1.0;
    v
}

#[tokio::test]
async fn create_upsert_query_delete_count_roundtrip() {
    let svc = SqliteVecService::open_in_memory().unwrap();
    svc.create_index(VectorIndexConfig {
        name: "docs".into(),
        model: "hand-crafted".into(),
        dimensions: 4,
        metric: DistanceMetric::Cosine,
        keyword_search: true,
    })
    .await
    .unwrap();

    let entries: Vec<VectorEntry> = (0..4)
        .map(|i| VectorEntry {
            id: format!("doc{i}"),
            vector: unit_vec(i),
            metadata: Some(serde_json::json!({"idx": i})),
            text: Some(format!("document number {i} cats dogs")),
        })
        .collect();
    svc.upsert("docs", entries).await.unwrap();

    assert_eq!(svc.count("docs").await.unwrap(), 4);

    // Vector-only query: probe near doc0's unit-vec; doc0 must rank first.
    let hits = svc
        .query("docs", unit_vec(0), 2, None, SearchMode::Vector, None)
        .await
        .unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits[0].id, "doc0");

    // Keyword query: bm25 against FTS5; "cats" appears in every doc, so
    // at least one hit must come back.
    let kw_hits = svc
        .query(
            "docs",
            unit_vec(0),
            3,
            None,
            SearchMode::Keyword,
            Some("cats".into()),
        )
        .await
        .unwrap();
    assert!(!kw_hits.is_empty());

    // Re-upsert doc0 with a fresh vector + metadata to exercise the
    // "existing rowid found, delete-then-reinsert into vec" branch.
    svc.upsert(
        "docs",
        vec![VectorEntry {
            id: "doc0".into(),
            vector: unit_vec(1),
            metadata: Some(serde_json::json!({"idx": 0, "updated": true})),
            text: Some("doc0 updated cats".into()),
        }],
    )
    .await
    .unwrap();
    assert_eq!(svc.count("docs").await.unwrap(), 4, "re-upsert keeps count");

    // Delete by id-list (uses the IN-clause builders).
    svc.delete("docs", vec!["doc0".into(), "doc2".into()])
        .await
        .unwrap();
    assert_eq!(svc.count("docs").await.unwrap(), 2);

    // delete_index drops all three tables.
    svc.delete_index("docs").await.unwrap();
}
