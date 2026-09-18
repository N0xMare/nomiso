//! Standalone toolkit composition: nomiso-memory used directly, with no
//! product crate (Vegapunk) on the dependency path. This is the T3 exit gate —
//! mechanics compose against `NomisoClient` + `MemoryPolicy` alone.

use nomiso_blob::BlobStore;
use nomiso_memory::{
    apply_ops, hard_recall, pack_context, preflight_ops, recall, CheckpointInput,
    HardRecallOptions, MemoryPolicy, RecallOptions, RememberInput, RuleWriter, SleepOptions,
    WriteEpisode, WriterOp,
};
use nomiso_service::NomisoClient;
use nomiso_store::StoreConfig;

async fn client() -> NomisoClient {
    NomisoClient::connect(StoreConfig::memory_test(8))
        .await
        .expect("connect")
}

#[tokio::test]
async fn toolkit_roundtrip_without_product_crate() {
    let client = client().await;
    let policy = MemoryPolicy {
        default_recall_limit: 4,
        ..MemoryPolicy::default()
    };
    let scope = "org/toolkit";

    // Typed writer path: preflight + prefix-preserving apply.
    let ops = vec![
        WriterOp::Put {
            input: RememberInput::fact(scope, "the toolkit composes without vegapunk"),
        },
        WriterOp::Put {
            input: RememberInput::fact(scope, "packs are explicit, never injected"),
        },
    ];
    preflight_ops(scope, policy, &ops).expect("preflight");
    let outcomes = apply_ops(&client, policy, scope, &ops)
        .await
        .expect("apply_ops");
    assert!(outcomes.iter().all(|o| o.is_ok()));

    // Reader path: recall + explicit pack — host decides whether to inject.
    let hits = recall(
        &client,
        scope,
        "toolkit composes",
        RecallOptions::from_policy(policy),
    )
    .await
    .expect("recall");
    assert!(!hits.is_empty());
    let pack = pack_context(&hits, 4, 4096);
    assert!(!pack.block.is_empty());

    let hr = hard_recall(
        &client,
        scope,
        "explicit packs",
        HardRecallOptions::from_policy(policy),
        &nomiso_memory::RuleQueryRewriter,
    )
    .await
    .expect("hard_recall");
    assert!(!hr.hits.is_empty());
}

#[tokio::test]
async fn toolkit_writer_and_checkpoint_and_sleep() {
    let client = client().await;
    let policy = MemoryPolicy::default();
    let scope = "org/toolkit2";

    // BYOM writer mechanics: RuleWriter extracts, apply_ops commits.
    let episode = WriteEpisode {
        scope: scope.into(),
        text: "first durable line\nsecond durable line".into(),
        source: Some("test".into()),
    };
    let outcomes = nomiso_memory::write_episode(&client, policy, &RuleWriter, &episode)
        .await
        .expect("write_episode");
    assert!(outcomes.iter().all(|o| o.is_ok()));

    // Checkpoint mechanics with a long-enough summary.
    let cp = nomiso_memory::checkpoint(
        &client,
        policy,
        CheckpointInput {
            scope: scope.into(),
            summary: "extracted two durable lines and committed them".into(),
            force: false,
        },
    )
    .await
    .expect("checkpoint");
    assert!(matches!(
        cp,
        nomiso_memory::CheckpointOutcome::Stored { .. }
            | nomiso_memory::CheckpointOutcome::Skipped { .. }
    ));

    // Sleep pass is proposal-producing, review-gated.
    let report = nomiso_memory::sleep_pass(&client, scope, SleepOptions::default())
        .await
        .expect("sleep_pass");
    assert!(report.applied.is_empty() || report.proposals.len() >= report.applied.len());
}

#[tokio::test]
async fn toolkit_artifact_relocation_fs_to_fs() {
    let client = client().await;
    let scope = "org/reloc";
    let src_dir = tempfile::tempdir().unwrap();
    let dst_dir = tempfile::tempdir().unwrap();
    let src = nomiso_blob::FsBlobStore::new(src_dir.path());
    let dst = nomiso_blob::FsBlobStore::new(dst_dir.path());

    // Two artifacts land in src; one will be corrupted mid-run.
    let a = nomiso_memory::artifact::store_artifact(
        &client,
        &src,
        nomiso_memory::StoreArtifactInput {
            scope: scope.into(),
            bytes: b"evidence-a".to_vec(),
            media_type: "text/plain".into(),
            source: None,
            trust: None,
        },
    )
    .await
    .unwrap();
    let b = nomiso_memory::artifact::store_artifact(
        &client,
        &src,
        nomiso_memory::StoreArtifactInput {
            scope: scope.into(),
            bytes: b"evidence-b".to_vec(),
            media_type: "text/plain".into(),
            source: None,
            trust: None,
        },
    )
    .await
    .unwrap();

    // Corrupt b's object in the source: relocation must report the failure
    // and leave the row pointing at the source.
    let b_path = src_dir
        .path()
        .join(&b.blake3[0..2])
        .join(&b.blake3[2..4])
        .join(&b.blake3);
    std::fs::write(&b_path, b"tampered").unwrap();

    let report = nomiso_memory::relocate_artifacts(&client, &src, &dst, Some(scope))
        .await
        .unwrap();
    assert_eq!(report.scanned, 2);
    assert_eq!(report.relocated, 1, "{report:?}");
    assert_eq!(report.failed, 1, "{report:?}");

    // a's row now resolves to the dst location and serves the same bytes.
    let arts = client.list_artifacts(Some(scope)).await.unwrap();
    let a_row = arts.iter().find(|r| r.id == a.id).unwrap();
    assert_ne!(a_row.location, a.location);
    assert!(a_row.location.contains(dst_dir.path().to_str().unwrap()));
    assert_eq!(dst.get(&a_row.location).await.unwrap(), b"evidence-a");
    // b's row is untouched — still points at the (corrupt) source.
    let b_row = arts.iter().find(|r| r.id == b.id).unwrap();
    assert_eq!(b_row.location, b.location);
}
