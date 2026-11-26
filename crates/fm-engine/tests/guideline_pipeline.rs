//! #72: lesson/guideline pipeline — caller-distilled lessons stored as
//! memories, merged into per-failure-mode guidelines, gated live by the
//! regression-tax gate (net gain > 0), injected via list_guidelines.
//! Boundary: tap/harness mine trajectories and measure success — memory
//! owns storage, merge, gate policy, injection surface (zero model calls).

use std::sync::Arc;

use fm_core::{FusionMemoryEngine as _, MemoryType};
use fm_embed::StubEmbedder;
use fm_engine::MemoryEngine;
use fm_persist::Persist;
use fm_store::LocalStore;

fn setup() -> (MemoryEngine, Arc<Persist>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(LocalStore::open(dir.path(), 8).unwrap());
    let persist = Arc::new(Persist::open_in_memory().unwrap());
    let embedder: Arc<dyn fm_embed::Embedder> = Arc::new(StubEmbedder::new(8));
    let engine = MemoryEngine::new(store, persist.clone(), embedder);
    (engine, persist)
}

fn episode(session: &str, mode: &str, lesson: &str) -> fm_core::DistillEpisode {
    fm_core::DistillEpisode {
        episode_id: format!("tap-{session}"),
        session_id: session.into(),
        failure_mode: mode.into(),
        lesson: lesson.into(),
    }
}

// Distill → promote → evaluate(pass) → live guideline visible on the
// injection surface with merge_count and lesson provenance.
#[tokio::test]
async fn pipeline_pass_gate_goes_live() {
    let (engine, persist) = setup();
    let ids = engine
        .distill_lessons(
            vec![
                episode(
                    "s1",
                    "pagination",
                    "always read the next_page token before re-querying",
                ),
                episode(
                    "s2",
                    "pagination",
                    "stop pagination on empty cursor, not on row count",
                ),
            ],
            "",
        )
        .await
        .unwrap();
    assert_eq!(ids.len(), 2);

    let entry = engine
        .promote_guideline("pagination", None, "")
        .await
        .unwrap();
    assert_eq!(entry.status, "trial");
    assert_eq!(entry.merge_count, 2);
    assert_eq!(entry.sources, ids);
    assert!(entry.content.contains("next_page"));

    // Trial rows hidden from the live injection surface.
    let live: Vec<_> = engine.list_guidelines(Some("live"), 50, "").await.unwrap();
    assert!(live.is_empty());

    // Regression-tax gate: net gain > 0 → live.
    let v = engine
        .evaluate_guideline(&entry.id, 0.9, 0.6, 0.05, "")
        .await
        .unwrap();
    assert_eq!(v.status, "live");
    assert!((v.net_gain - 0.25).abs() < 1e-9);

    let live = engine.list_guidelines(Some("live"), 50, "").await.unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].id, entry.id);
    assert_eq!(live[0].merge_count, 2);
    assert_eq!(live[0].failure_mode, "pagination");
}

// Gate fail path: regressions eat the success improvement → rejected, and a
// rejected guideline refuses re-promotion (fail-safe, no silent revival).
#[tokio::test]
async fn pipeline_fail_gate_rejects_and_blocks_repromote() {
    let (engine, _persist) = setup();
    engine
        .distill_lessons(
            vec![episode(
                "s1",
                "entity-resolution",
                "match entities on id, never on display name",
            )],
            "",
        )
        .await
        .unwrap();
    let entry = engine
        .promote_guideline("entity-resolution", None, "")
        .await
        .unwrap();

    let v = engine
        .evaluate_guideline(&entry.id, 0.8, 0.7, 0.2, "")
        .await
        .unwrap();
    assert_eq!(v.status, "rejected");
    assert!((v.net_gain - (-0.1)).abs() < 1e-9);

    let live = engine.list_guidelines(Some("live"), 50, "").await.unwrap();
    assert!(live.is_empty());
    // Rejected visible under its own status (audit surface).
    let rejected = engine
        .list_guidelines(Some("rejected"), 50, "")
        .await
        .unwrap();
    assert_eq!(rejected.len(), 1);

    // Re-promotion over the rejected guideline is refused.
    let err = engine
        .promote_guideline("entity-resolution", None, "")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("regression-tax gate"), "{err}");
    // Re-evaluating a rejected (non-trial) guideline also refuses.
    let err = engine
        .evaluate_guideline(&entry.id, 0.9, 0.5, 0.0, "")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a trial guideline"), "{err}");
}

// Re-promote after new lessons updates the existing trial guideline
// (merge_count + sources grow, id stable) — idempotent merge semantics.
#[tokio::test]
async fn promote_is_idempotent_and_absorbs_new_lessons() {
    let (engine, _persist) = setup();
    engine
        .distill_lessons(
            vec![episode(
                "s1",
                "no-evidence",
                "never answer without a cited source",
            )],
            "",
        )
        .await
        .unwrap();
    let e1 = engine
        .promote_guideline("no-evidence", None, "")
        .await
        .unwrap();
    assert_eq!(e1.merge_count, 1);

    engine
        .distill_lessons(
            vec![episode(
                "s2",
                "no-evidence",
                "quote the evidence line before the answer",
            )],
            "",
        )
        .await
        .unwrap();
    let e2 = engine
        .promote_guideline("no-evidence", None, "")
        .await
        .unwrap();
    assert_eq!(e2.id, e1.id);
    assert_eq!(e2.merge_count, 2);
    assert_eq!(e2.sources.len(), 2);
    assert_eq!(e2.status, "trial");
}

// Empty failure mode (no lessons) and invalid inputs fail closed.
#[tokio::test]
async fn input_validation_fails_closed() {
    let (engine, _persist) = setup();
    let err = engine
        .promote_guideline("ghost-mode", None, "")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no lessons"), "{err}");

    let err = engine
        .distill_lessons(vec![episode("s1", "pagination", "   ")], "")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("non-empty"), "{err}");

    let err = engine.distill_lessons(vec![], "").await.unwrap_err();
    assert!(err.to_string().contains("empty"), "{err}");

    engine
        .distill_lessons(vec![episode("s1", "pagination", "read the cursor")], "")
        .await
        .unwrap();
    let entry = engine
        .promote_guideline("pagination", None, "")
        .await
        .unwrap();
    // Out-of-range gate inputs refused before any verdict.
    let err = engine
        .evaluate_guideline(&entry.id, 1.5, 0.5, 0.0, "")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("[0,1]"), "{err}");
    // Bad status filter and limit refused.
    let err = engine
        .list_guidelines(Some("bogus"), 50, "")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("invalid status"), "{err}");
    let err = engine.list_guidelines(None, 0, "").await.unwrap_err();
    assert!(err.to_string().contains("limit"), "{err}");
}

// Tenant scoping: lessons and guidelines in tenant A are invisible to B.
#[tokio::test]
async fn tenant_scoped_isolation() {
    let (engine, _persist) = setup();
    engine
        .distill_lessons(
            vec![episode("s1", "pagination", "cursor first, query second")],
            "tenant-A",
        )
        .await
        .unwrap();
    let entry = engine
        .promote_guideline("pagination", None, "tenant-A")
        .await
        .unwrap();

    // Tenant B sees no lessons and no guidelines.
    let err = engine
        .promote_guideline("pagination", None, "tenant-B")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no lessons"), "{err}");
    let rows = engine.list_guidelines(None, 50, "tenant-B").await.unwrap();
    assert!(rows.is_empty());
    // Gate cross-tenant read hides the guideline (NotFound, no existence leak).
    let err = engine
        .evaluate_guideline(&entry.id, 0.9, 0.5, 0.0, "tenant-B")
        .await
        .unwrap_err();
    assert!(matches!(err, fm_core::MemoryError::NotFound(_)), "{err}");

    // Owner tenant still sees it.
    let rows = engine
        .list_guidelines(Some("trial"), 50, "tenant-A")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
}

// Lessons are storage rows (Episodic, provenance=lesson, failure_mode set),
// guidelines Semantic with status — never leaking into other memory flows.
#[tokio::test]
async fn lesson_and_guideline_row_shapes() {
    let (engine, persist) = setup();
    let ids = engine
        .distill_lessons(vec![episode("s1", "pagination", "cursor first")], "")
        .await
        .unwrap();
    let entry = engine
        .promote_guideline("pagination", Some("one merged rule"), "")
        .await
        .unwrap();
    assert_eq!(
        entry.content, "one merged rule",
        "caller content overrides the joined default"
    );
    let lesson = persist.get_memory(&ids[0]).unwrap().unwrap();
    assert!(matches!(lesson.memory_type, MemoryType::Episodic));
    assert_eq!(lesson.provenance.as_deref(), Some("lesson"));
    assert_eq!(lesson.failure_mode.as_deref(), Some("pagination"));
    assert!(lesson.guideline_status.is_none());
    let guide = persist.get_memory(&entry.id).unwrap().unwrap();
    assert!(matches!(guide.memory_type, MemoryType::Semantic));
    assert_eq!(guide.guideline_status.as_deref(), Some("trial"));
    assert_eq!(guide.merge_count, 1);
}
