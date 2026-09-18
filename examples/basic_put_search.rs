//! Basic put + BM25 search against in-memory SurrealDB.
//!
//! ```bash
//! cargo run -p nomiso --example basic_put_search --features embedded-mem
//! ```

use nomiso::types::{Category, Content};
use nomiso::{NomisoClient, PutRequest, SearchQuery, StoreConfig};

#[tokio::main]
async fn main() -> nomiso::Result<()> {
    let client = NomisoClient::connect(StoreConfig::memory_test(8)).await?;
    client
        .put(PutRequest {
            scope: "org/demo/user/alice".into(),
            category: Category::Semantic,
            content: Content::text("Alice prefers TypeScript for agent tooling."),
            valid_from: None,
            valid_until: None,
            known_at: None,
            confidence: Some(0.95),
            provenance: Default::default(),
            entity_links: vec![],
            embedding: None,
            embedding_identity: None,
        idempotency_key: None,
        extractor_version: None,
        model_version: None,
        valid_rev_from: None,
        valid_rev_until: None,
        })
        .await?;

    let hits = client
        .search(SearchQuery {
            query: "TypeScript agent tooling".into(),
            scope: "org/demo/user/alice".into(),
            scope_match: Default::default(),
            as_of: None,
            known_as_of: None,
            sys_as_of: None,
            categories: None,
            limit: Some(5),
            embedding: None,
            graph_enrich: Some(false),
        })
        .await?;

    println!("hits: {}", hits.len());
    for h in hits {
        println!("  [{:.4}] {} — {}", h.score, h.id, h.preview);
    }
    Ok(())
}
