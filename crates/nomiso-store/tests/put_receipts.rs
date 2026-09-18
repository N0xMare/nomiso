use nomiso_core::{Category, Content, Error, ForgetRequest, MemoryStore, PutRequest};
use nomiso_store::{StoreConfig, SurrealMemoryStore};

async fn store() -> SurrealMemoryStore {
    let store = SurrealMemoryStore::connect(StoreConfig::memory_test(8))
        .await
        .unwrap();
    store.migrate().await.unwrap();
    store
}

fn request(text: &str) -> PutRequest {
    PutRequest {
        scope: "org/receipt".into(),
        category: Category::Semantic,
        content: Content::text(text),
        idempotency_key: Some("operation-1".into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn receipt_rejects_changed_input() {
    let store = store().await;
    store.put(request("original fact")).await.unwrap();
    let error = store
        .put(request("different fact"))
        .await
        .expect_err("key reuse must not accept changed input");
    assert!(matches!(error, Error::IdempotencyConflict));
}

#[tokio::test]
async fn receipt_checks_all_caller_supplied_content() {
    let store = store().await;
    store.put(request("original fact")).await.unwrap();
    let mut changes = Vec::new();
    let mut req = request("original fact");
    req.content.attrs = Some(serde_json::json!({"setting": 2}));
    changes.push(req);
    let mut req = request("original fact");
    req.category = Category::Episodic;
    changes.push(req);
    let mut req = request("original fact");
    req.embedding = Some(vec![1.0; 8]);
    changes.push(req);
    let mut req = request("original fact");
    req.valid_from = Some("2000-01-01T00:00:00Z".parse().unwrap());
    changes.push(req);
    for req in changes {
        assert!(matches!(
            store.put(req).await,
            Err(Error::IdempotencyConflict)
        ));
    }
}

#[tokio::test]
async fn receipt_normalizes_nested_object_order() {
    let store = store().await;
    let mut req = request("original fact");
    req.content.attrs = Some(serde_json::from_str(r#"{"z":{"b":2,"a":1},"a":0}"#).unwrap());
    let first = store.put(req.clone()).await.unwrap();
    req.content.attrs = Some(serde_json::from_str(r#"{"a":0,"z":{"a":1,"b":2}}"#).unwrap());
    let replay = store.put(req).await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.id, first.id);
}

#[tokio::test]
async fn competing_different_requests_do_not_share_success() {
    let store = store().await;
    let mut tasks = Vec::new();
    for i in 0..8 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            store.put(request(&format!("distinct {i}"))).await
        }));
    }
    let mut committed = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(receipt) => {
                assert!(!receipt.replayed);
                committed += 1;
            }
            Err(Error::IdempotencyConflict) => {}
            other => panic!("unexpected result: {other:?}"),
        }
    }
    assert_eq!(committed, 1);
}

#[tokio::test]
async fn receipt_returns_original_ack_after_soft_forget() {
    let store = store().await;
    let first = store.put(request("original fact")).await.unwrap();
    store
        .forget(ForgetRequest {
            id: first.id.clone(),
            scope: "org/receipt".into(),
            expected_version: Some(first.version),
            hard: false,
            at: None,
        })
        .await
        .unwrap();
    let replay = store.put(request("original fact")).await.unwrap();
    assert_eq!(replay.id, first.id);
    assert_eq!(replay.version, first.version);
    assert_eq!(replay.record, first.record);
}

#[tokio::test]
async fn receipt_cannot_resurrect_erased_memory() {
    let store = store().await;
    let first = store.put(request("original fact")).await.unwrap();
    store
        .forget(ForgetRequest {
            id: first.id,
            scope: "org/receipt".into(),
            expected_version: Some(first.version),
            hard: true,
            at: None,
        })
        .await
        .unwrap();
    let error = store.put(request("original fact")).await.unwrap_err();
    assert!(matches!(error, Error::IdempotencyUnavailable));
}
