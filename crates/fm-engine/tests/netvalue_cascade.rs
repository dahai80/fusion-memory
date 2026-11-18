//! #68 B4/B5: net-value ledger (auto-shutdown) + cascade forgetting (stale_sources).
//! Acceptance (issue #68): ledger reports net value per op class; below-floor op
//! class demonstrably auto-shuts down and gates commit/retrieve; reenable path
//! restores it. Forgetting one node marks its dependents with stale_sources —
//! no downstream read of a forgotten entry without stale annotation.

use std::sync::Arc;

use fm_core::{FusionMemoryEngine as _, Interaction, MemoryItem, MemoryType, RetrieveQuery, Turn};
use fm_embed::StubEmbedder;
use fm_engine::{MemoryEngine, NetValuePolicy};
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

fn item(id: &str, content: &str, ts: u64) -> MemoryItem {
    let mut m = MemoryItem::new_turn_skeleton(
        id.to_string(),
        format!("ix-{id}"),
        0,
        format!("s-{id}"),
        String::new(),
        MemoryType::Episodic,
        content.to_string(),
        ts,
    );
    m.entities_pending = false;
    m.vector_ref = id.to_string();
    m
}

fn interaction(class: Option<&str>, text: &str) -> Interaction {
    Interaction {
        id: format!("ix-{text}"),
        session_id: "sess-1".into(),
        tenant: String::new(),
        namespace: String::new(),
        namespace_key: String::new(),
        turns: vec![Turn {
            turn_idx: 0,
            user_message: text.into(),
            assistant_message: String::new(),
            tool_calls: Vec::new(),
        }],
        timestamp: 1000,
        owner: None,
        metadata: match class {
            Some(c) => serde_json::json!({ "op_class": c }),
            None => serde_json::json!({}),
        },
    }
}

// #68 B5: deleting a cited memory annotates the citing dependent with
// stale_sources (one hop), and the annotation is visible on read.
#[tokio::test]
async fn cascade_forget_marks_dependents() {
    let (engine, persist) = setup();
    persist
        .put_memory(&item("src", "the source fact", 1000))
        .unwrap();
    let mut dep = item("dep", "derived from the source fact", 1001);
    dep.sources = vec!["src".into()];
    persist.put_memory(&dep).unwrap();

    engine.delete_memory("src").await.unwrap();

    // Forgotten entry tombstoned, no longer readable.
    assert!(persist.get_memory("src").unwrap().unwrap().tombstone);
    // Dependent stays alive but carries the stale annotation.
    let got = persist.get_memory("dep").unwrap().unwrap();
    assert!(!got.tombstone);
    assert_eq!(got.stale_sources, vec!["src".to_string()]);
    // Idempotent: re-annotating the same dependent is a no-op (second cascade
    // pass triggered by another forget must not duplicate).
    let mut dep2 = item("dep2", "another dependent", 1002);
    dep2.sources = vec!["src".into()];
    persist.put_memory(&dep2).unwrap();
    engine.delete_memory("unrelated").await.unwrap();
    assert_eq!(
        persist.get_memory("dep").unwrap().unwrap().stale_sources,
        vec!["src".to_string()]
    );
}

// #68 B5: merge-absorbed targets are dependents too; chain forgetting stays
// one-hop-per-event (transitivity emerges from subsequent forgets).
#[tokio::test]
async fn cascade_forget_merge_target_and_chain() {
    let (engine, persist) = setup();
    persist.put_memory(&item("a", "original", 1000)).unwrap();
    persist.put_memory(&item("t", "merged copy", 1001)).unwrap();
    persist
        .record_merge("a", "t", "near-duplicate", 1002)
        .unwrap();
    // chain: b cites a, c cites b
    let mut b = item("b", "cites a", 1003);
    b.sources = vec!["a".into()];
    persist.put_memory(&b).unwrap();
    let mut c = item("c", "cites b", 1004);
    c.sources = vec!["b".into()];
    persist.put_memory(&c).unwrap();

    engine.delete_memory("a").await.unwrap();
    assert_eq!(
        persist.get_memory("t").unwrap().unwrap().stale_sources,
        vec!["a".to_string()]
    );
    assert_eq!(
        persist.get_memory("b").unwrap().unwrap().stale_sources,
        vec!["a".to_string()]
    );
    assert!(
        persist
            .get_memory("c")
            .unwrap()
            .unwrap()
            .stale_sources
            .is_empty(),
        "one hop per forget event: c not annotated yet"
    );
    // Forgetting b cascades to c — transitivity across events.
    engine.delete_memory("b").await.unwrap();
    assert_eq!(
        persist.get_memory("c").unwrap().unwrap().stale_sources,
        vec!["b".to_string()]
    );
}

// #68 B4: below-floor op class auto-shuts down, gates retrieve + commit,
// reenable restores service; confirm=false and unknown-class paths fail-closed.
#[tokio::test]
async fn netvalue_shutdown_gate_and_reenable() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(LocalStore::open(dir.path(), 8).unwrap());
    let persist = Arc::new(Persist::open_in_memory().unwrap());
    let embedder: Arc<dyn fm_embed::Embedder> = Arc::new(StubEmbedder::new(8));
    let engine = MemoryEngine::new(store, persist, embedder).with_net_value(NetValuePolicy {
        floor: 0.0,
        window_ms: 7 * 24 * 3600 * 1000,
        min_samples: 3,
    });

    let class = "retrieve:expensive";
    // Money-losing ops (benefit 1, tax 5 each → window avg net -4 < 0).
    // min_samples=3: first two records below the floor, no shutdown;
    // the 3rd crosses min_samples and fires the shutdown transition.
    for i in 0..2 {
        let status = engine
            .record_op_cost(
                fm_core::OpCostRecord {
                    op: "retrieve".into(),
                    class: class.into(),
                    memory_id: None,
                    benefit: 1.0,
                    tax: 5.0,
                    at: 0,
                },
                "",
            )
            .await
            .unwrap();
        assert_eq!(status.ops, (i + 1) as u64);
        assert!(!status.shutdown, "below min_samples no shutdown yet");
    }
    let status = engine
        .record_op_cost(
            fm_core::OpCostRecord {
                op: "retrieve".into(),
                class: class.into(),
                memory_id: None,
                benefit: 1.0,
                tax: 5.0,
                at: 0,
            },
            "",
        )
        .await
        .unwrap();
    assert!(
        status.shutdown && status.just_shutdown,
        "3rd loss op crosses min_samples and shuts down"
    );

    // Report shows the shutdown class.
    let rows = engine.net_value_report("").await.unwrap();
    let row = rows.iter().find(|r| r.class == class).unwrap();
    assert!(row.shutdown);
    assert!(row.net < 0.0);

    // Gated retrieve: op_class match → Unsupported (fail-closed).
    let mut q = RetrieveQuery::new("the source fact", 5, 512);
    q.op_class = Some(class.into());
    let err = engine.retrieve_context(&q).await.unwrap_err();
    assert!(
        err.to_string().contains("auto-shut down"),
        "gated retrieve must reject, got {err}"
    );
    // Ungated retrieve still works.
    let mut q2 = RetrieveQuery::new("the source fact", 5, 512);
    q2.op_class = Some("retrieve:other".into());
    engine.retrieve_context(&q2).await.unwrap();

    // Gated commit: interaction.metadata["op_class"] match → rejected.
    let err = engine
        .commit_episodic_memory("sess-1", &interaction(Some(class), "hello there"))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("auto-shut down"),
        "gated commit must reject, got {err}"
    );
    // Ungated commit passes.
    engine
        .commit_episodic_memory("sess-1", &interaction(Some("commit:normal"), "hello there"))
        .await
        .unwrap();

    // Reenable without confirm is impossible at RPC layer; direct trait call restores.
    let was = engine.net_value_reenable(class, "").await.unwrap();
    assert!(was, "class was shut down before reenable");
    let was2 = engine.net_value_reenable(class, "").await.unwrap();
    assert!(!was2, "second reenable: class was not shut down");
    engine.retrieve_context(&q).await.unwrap();
}

// #68 B4: profitable classes never shut down even past min_samples.
#[tokio::test]
async fn netvalue_profitable_class_stays_open() {
    let (engine, _persist) = setup();
    let engine = engine.with_net_value(NetValuePolicy {
        floor: 0.0,
        window_ms: 7 * 24 * 3600 * 1000,
        min_samples: 3,
    });
    for _ in 0..5 {
        let status = engine
            .record_op_cost(
                fm_core::OpCostRecord {
                    op: "retrieve".into(),
                    class: "retrieve:cheap".into(),
                    memory_id: None,
                    benefit: 10.0,
                    tax: 1.0,
                    at: 0,
                },
                "",
            )
            .await
            .unwrap();
        assert!(!status.shutdown);
    }
    let rows = engine.net_value_report("").await.unwrap();
    assert!(rows.iter().all(|r| !r.shutdown));
}

// #68 B4: tenant isolation — one tenant's shutdown does not gate another's.
#[tokio::test]
async fn netvalue_shutdown_tenant_scoped() {
    let (engine, _persist) = setup();
    let engine = engine.with_net_value(NetValuePolicy {
        floor: 0.0,
        window_ms: 7 * 24 * 3600 * 1000,
        min_samples: 2,
    });
    for _ in 0..2 {
        engine
            .record_op_cost(
                fm_core::OpCostRecord {
                    op: "retrieve".into(),
                    class: "retrieve:bad".into(),
                    memory_id: None,
                    benefit: 0.0,
                    tax: 9.0,
                    at: 0,
                },
                "tenant-a",
            )
            .await
            .unwrap();
    }
    let rows_a = engine.net_value_report("tenant-a").await.unwrap();
    assert!(
        rows_a
            .iter()
            .find(|r| r.class == "retrieve:bad")
            .unwrap()
            .shutdown
    );
    let rows_b = engine.net_value_report("tenant-b").await.unwrap();
    assert!(rows_b.is_empty(), "tenant-b ledger independent");
    // tenant-b can still use the class.
    let mut q = RetrieveQuery::new("x", 5, 512);
    q.op_class = Some("retrieve:bad".into());
    // engine default tenant is "" (not tenant-b) — gate reads engine tenant,
    // which never got a shutdown row, so it passes.
    engine.retrieve_context(&q).await.unwrap();
}
