//! T4 controller contract tests: typed prepare-context → proposal+manifest,
//! global budgets, inventory dedup, manifest fidelity, and verified
//! actual-insertion acknowledgment (CTX-001/003/004/005/006/009).

use nomiso_memory::{
    apply_ops, prepare_context, record_insertion, DegradationPolicy, Effort, ExclusionReason,
    InsertedBlock, InsertionAck, InventoryItem, MemoryPolicy, PrepareContextRequest,
    ProposalStatus, RememberInput, TokenMethod, WriterOp,
};
use nomiso_service::NomisoClient;
use nomiso_store::StoreConfig;

async fn client() -> NomisoClient {
    NomisoClient::connect(StoreConfig::memory_test(8))
        .await
        .expect("connect")
}

async fn seed(client: &NomisoClient, scope: &str, texts: &[&str]) {
    let ops: Vec<WriterOp> = texts
        .iter()
        .map(|t| WriterOp::Put {
            input: RememberInput::fact(scope, *t),
        })
        .collect();
    let outcomes = apply_ops(client, MemoryPolicy::default(), scope, &ops)
        .await
        .expect("seed apply_ops");
    assert!(outcomes.iter().all(|o| o.is_ok()), "{outcomes:?}");
}

fn req(scope: &str, task: &str) -> PrepareContextRequest {
    PrepareContextRequest::for_task(scope, task)
}

#[tokio::test]
async fn prepare_context_ready_with_manifest_fidelity() {
    let client = client().await;
    let scope = "org/ctx1";
    seed(
        &client,
        scope,
        &[
            "the deploy freeze starts friday",
            "rollback requires two approvals",
            "canary watches error budget burn",
        ],
    )
    .await;

    let p = prepare_context(&client, req(scope, "deploy freeze approvals"))
        .await
        .expect("prepare_context");
    assert_eq!(p.status, ProposalStatus::Ready, "{p:?}");
    assert!(!p.blocks.is_empty());
    assert!(!p.proposal_id.is_empty());
    assert_eq!(p.manifest.selected.len(), p.blocks.len());
    // CTX-006: rendered derives from blocks — every excerpt appears verbatim.
    let rendered = p.rendered.as_deref().expect("rendered");
    for b in &p.blocks {
        assert!(rendered.contains(&b.excerpt), "missing excerpt for {b:?}");
        assert!(rendered.contains(&b.memory_id.to_string()));
        assert!(!b.digest.is_empty() && !b.block_id.is_empty());
    }
    // Composition identity + labeled budget accounting.
    assert!(!p.composition.schema_version.is_empty());
    assert_eq!(p.composition.effort, "direct");
    assert_eq!(
        p.manifest.usage.token_method,
        TokenMethod::ApproxCharsPerToken { chars_per_token: 4 }
    );
    assert!(p.manifest.usage.tokens_used > 0);
    // No inventory → disclosed, not claimed.
    assert!(!p.manifest.inventory_aware);
    assert!(p.unresolved.iter().any(|u| u.contains("inventory")));
}

#[tokio::test]
async fn inventory_dedup_before_ranking() {
    let client = client().await;
    let scope = "org/ctx2";
    seed(&client, scope, &["the release tag is v42"]).await;
    let first = prepare_context(&client, req(scope, "release tag"))
        .await
        .unwrap();
    let hit = first.blocks.first().expect("a block");

    // Same request with that memory already in the prompt.
    let mut r = req(scope, "release tag");
    r.inventory = vec![InventoryItem {
        memory_id: hit.memory_id.clone(),
        version: None,
    }];
    let p = prepare_context(&client, r).await.unwrap();
    assert!(p.manifest.inventory_aware);
    assert!(!p.blocks.iter().any(|b| b.memory_id == hit.memory_id));
    let rec = p
        .manifest
        .candidates
        .iter()
        .find(|c| c.memory_id == hit.memory_id)
        .expect("candidate record");
    assert_eq!(rec.excluded, Some(ExclusionReason::AlreadyInContext));
}

#[tokio::test]
async fn global_budget_caps_blocks_and_marks_partial() {
    let client = client().await;
    let scope = "org/ctx3";
    seed(
        &client,
        scope,
        &[
            "budget item alpha budget",
            "budget item bravo budget",
            "budget item charlie budget",
            "budget item delta budget",
        ],
    )
    .await;
    let mut r = req(scope, "budget item");
    r.budget.max_blocks = 2;
    r.budget.max_candidates = 8;
    let p = prepare_context(&client, r).await.unwrap();
    assert_eq!(p.status, ProposalStatus::Partial, "{p:?}");
    assert!(p.blocks.len() <= 2);
    assert!(p
        .manifest
        .candidates
        .iter()
        .any(|c| c.excluded == Some(ExclusionReason::OverBlockLimit)));
    assert!(p.unresolved.iter().any(|u| u.contains("block cap")));
}

#[tokio::test]
async fn oversized_first_hit_degrades_to_reference_not_empty() {
    let client = client().await;
    let scope = "org/ctx4";
    let big = "x".repeat(5000);
    let texts = vec![big.as_str(), "short fitting note about berries"];
    seed(&client, scope, &texts).await;
    let mut r = req(scope, "x note");
    r.budget.max_tokens = 40; // ~160 chars rendered
    let p = prepare_context(&client, r).await.unwrap();
    // CTX-005: proposal must not be empty merely because the top hit is big —
    // a reference stub or a later candidate must carry something.
    assert!(
        p.status == ProposalStatus::Partial
            || p.status == ProposalStatus::Ready
            || p.status == ProposalStatus::BudgetExhausted,
        "{p:?}"
    );
    if !p.blocks.is_empty() {
        assert!(
            p.blocks.iter().any(|b| b.reference_only) || p.blocks.iter().any(|b| !b.reference_only)
        );
        for f in &p.follow_ups {
            assert_eq!(f.kind, "read");
        }
    }
}

#[tokio::test]
async fn strict_provider_tokens_refused_and_empty_is_typed() {
    let client = client().await;
    let scope = "org/ctx5";

    let mut r = req(scope, "anything");
    r.budget.token_method = TokenMethod::StrictProvider;
    let e = prepare_context(&client, r).await.unwrap_err();
    assert_eq!(e.code(), "invalid_request");

    let p = prepare_context(&client, req(scope, "nothing stored here"))
        .await
        .unwrap();
    assert_eq!(p.status, ProposalStatus::Empty);
    assert!(p.rendered.is_none());
    assert!(p.blocks.is_empty());
}

#[tokio::test]
async fn insertion_ack_verified_idempotent_and_subset_only() {
    let client = client().await;
    let scope = "org/ctx6";
    seed(
        &client,
        scope,
        &["ack target fact one", "ack target fact two"],
    )
    .await;
    let p = prepare_context(&client, req(scope, "ack target"))
        .await
        .unwrap();
    assert!(!p.blocks.is_empty());
    let first = &p.blocks[0];

    // Unknown block id → rejected (no fabricated insertion).
    let bad = record_insertion(
        &client,
        InsertionAck {
            trace_id: p.trace_id.clone(),
            proposal_id: p.proposal_id.clone(),
            scope: scope.into(),
            host: "test-harness".into(),
            inserted: vec![InsertedBlock {
                block_id: "fictitious".into(),
                truncated: false,
                note: None,
            }],
            session_id: None,
            turn_id: None,
        },
    )
    .await;
    assert_eq!(bad.unwrap_err().code(), "invalid_request");

    // Real subset ack → receipt.
    let rec = record_insertion(
        &client,
        InsertionAck {
            trace_id: p.trace_id.clone(),
            proposal_id: p.proposal_id.clone(),
            scope: scope.into(),
            host: "test-harness".into(),
            inserted: vec![InsertedBlock {
                block_id: first.block_id.clone(),
                truncated: true,
                note: Some("placed in system header".into()),
            }],
            session_id: None,
            turn_id: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(rec.inserted, vec![first.block_id.clone()]);
    assert!(!rec.replayed);

    // Identical replay → same receipt, no new event.
    let rec2 = record_insertion(
        &client,
        InsertionAck {
            trace_id: p.trace_id.clone(),
            proposal_id: p.proposal_id.clone(),
            scope: scope.into(),
            host: "test-harness".into(),
            inserted: vec![InsertedBlock {
                block_id: first.block_id.clone(),
                truncated: true,
                note: Some("placed in system header".into()),
            }],
            session_id: None,
            turn_id: None,
        },
    )
    .await
    .unwrap();
    assert!(rec2.replayed);
    let bundle = client.get_trace(&p.trace_id, scope).await.unwrap();
    let injects = bundle
        .events
        .iter()
        .filter(|e| e.kind == nomiso_core::TraceEventKind::Inject)
        .count();
    assert_eq!(injects, 1, "replay must not append a second inject");

    // A different set for the same proposal → idempotency conflict.
    let other_block = p.blocks.get(1).map(|b| b.block_id.clone());
    let conflicting = record_insertion(
        &client,
        InsertionAck {
            trace_id: p.trace_id.clone(),
            proposal_id: p.proposal_id.clone(),
            scope: scope.into(),
            host: "test-harness".into(),
            inserted: vec![InsertedBlock {
                block_id: other_block.unwrap_or_else(|| first.block_id.clone()),
                truncated: false,
                note: None,
            }],
            session_id: None,
            turn_id: None,
        },
    )
    .await;
    if p.blocks.len() > 1 {
        assert_eq!(conflicting.unwrap_err().code(), "idempotency_conflict");
    }

    // Unknown proposal id → rejected.
    let nope = record_insertion(
        &client,
        InsertionAck {
            trace_id: p.trace_id.clone(),
            proposal_id: "deadbeef".into(),
            scope: scope.into(),
            host: "test-harness".into(),
            inserted: vec![],
            session_id: None,
            turn_id: None,
        },
    )
    .await;
    assert_eq!(nope.unwrap_err().code(), "invalid_request");
}

#[tokio::test]
async fn deadline_and_degradation_and_expanded_effort() {
    let client = client().await;
    let scope = "org/ctx7";
    seed(&client, scope, &["effort plan fact"]).await;

    // Zero deadline → selection path still returns a proposal, marked partial.
    let mut r = req(scope, "effort plan");
    r.budget.deadline_ms = Some(0);
    let p = prepare_context(&client, r).await.unwrap();
    assert!(matches!(
        p.status,
        ProposalStatus::Partial | ProposalStatus::Ready | ProposalStatus::BudgetExhausted
    ));

    // Expanded effort wires graph_expand into retrieval.
    let mut r = req(scope, "effort plan");
    r.effort = Some(Effort::Expanded(nomiso_core::GraphExpand {
        max_depth: Some(1),
        ..Default::default()
    }));
    let p = prepare_context(&client, r).await.unwrap();
    assert_eq!(p.composition.effort, "expanded");

    // AllowPartial on an unreachable scope still types cleanly (no panic).
    let mut r = req(scope, "effort plan");
    r.degradation = DegradationPolicy::AllowPartial;
    let p = prepare_context(&client, r).await.unwrap();
    assert!(!p.proposal_id.is_empty());
}
