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
            confidence: Some(0.95),
            ..Default::default()
        })
        .await?;

    let hits = client
        .search(SearchQuery {
            query: "TypeScript agent tooling".into(),
            scope: "org/demo/user/alice".into(),
            limit: Some(5),
            graph_enrich: Some(false),
            ..Default::default()
        })
        .await?;

    println!("hits: {}", hits.len());
    for h in hits {
        println!("  [{:.4}] {} — {}", h.score, h.id, h.preview);
    }
    Ok(())
}
