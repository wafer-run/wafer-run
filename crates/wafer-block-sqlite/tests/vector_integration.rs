//! Integration: SqliteVecService + FastembedService roundtrip.
//!
//! Uses MiniLM (120 MB download on first run). Gate with
//! `SOLOBASE_RUN_FASTEMBED_TESTS=1` to enable.

#![cfg(feature = "vectors")]

use wafer_block_sqlite::vector::SqliteVecService;
use wafer_core::interfaces::vector::{
    service::{EmbeddingService, VectorService},
    DistanceMetric, SearchMode, VectorEntry, VectorIndexConfig,
};

#[tokio::test]
#[ignore]
async fn ingest_then_query_returns_ranked_matches() {
    if std::env::var("SOLOBASE_RUN_FASTEMBED_TESTS").is_err() {
        return;
    }
    use wafer_block_fastembed::FastembedService;

    let emb = FastembedService::new("paraphrase-multilingual-MiniLM-L12-v2").unwrap();
    let vec_svc = SqliteVecService::open_in_memory().unwrap();
    vec_svc
        .create_index(VectorIndexConfig {
            name: "docs".into(),
            model: "paraphrase-multilingual-MiniLM-L12-v2".into(),
            dimensions: 384,
            metric: DistanceMetric::Cosine,
            keyword_search: true,
        })
        .await
        .unwrap();

    let texts = vec![
        "Cats are small furry animals that purr and hunt mice.".to_string(),
        "Dogs are loyal companions that bark and fetch sticks.".to_string(),
        "Fish live underwater and breathe through gills.".to_string(),
    ];
    let vectors = emb.embed(texts.clone()).await.unwrap();

    let entries: Vec<VectorEntry> = texts
        .iter()
        .zip(vectors.iter())
        .enumerate()
        .map(|(i, (t, v))| VectorEntry {
            id: format!("doc{i}"),
            vector: v.clone(),
            metadata: Some(serde_json::json!({"source": "test"})),
            text: Some(t.clone()),
        })
        .collect();
    vec_svc.upsert("docs", entries).await.unwrap();

    let q_vec = emb
        .embed(vec!["a small pet that purrs".to_string()])
        .await
        .unwrap();
    let hits = vec_svc
        .query(
            "docs",
            q_vec[0].clone(),
            3,
            None,
            SearchMode::Hybrid,
            Some("purr".into()),
        )
        .await
        .unwrap();

    assert!(!hits.is_empty());
    assert_eq!(
        hits[0].id, "doc0",
        "cat doc should rank first for cat-related query"
    );
}
