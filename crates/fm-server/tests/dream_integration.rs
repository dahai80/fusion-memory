//! M2 #40: dreaming scheduler integration — real engine, real in-flight window,
//! real consolidate_lock reuse, boundary events in audit_log.

use std::sync::Arc;
use std::time::Duration;

use fm_core::{FusionMemoryEngine, Interaction, RetrieveQuery, Turn};
use fm_embed::{EmbedResult, Embedder, StubEmbedder};
use fm_engine::MemoryEngine;
use fm_persist::Persist;
use fm_server::dream::{dream_cycle_once, DreamConfig};

use fm_store::LocalStore;

/// StubEmbedder + 200ms hold — keeps the engine in-flight long enough to
/// observe the dream idle gate deterministically.
struct SlowEmbedder(StubEmbedder);

#[async_trait::async_trait]
impl Embedder for SlowEmbedder {
    async fn embed(&self, text: &str) -> EmbedResult<Vec<f32>> {
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.0.embed(text).await
    }
    fn dimension(&self) -> usize {
        self.0.dimension()
    }
    fn is_live(&self) -> bool {
        false
    }
}

struct TestRig {
    engine: Arc<MemoryEngine>,
    audit: Persist,
    _dir: tempfile::TempDir,
}

fn rig(dim: usize, slow: bool) -> TestRig {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(LocalStore::open(dir.path().join("store"), dim).unwrap());
    let persist = Arc::new(Persist::open(dir.path().join("memory.db")).unwrap());
    let audit = Persist::open(dir.path().join("memory.db")).unwrap();
    let embedder: Arc<dyn Embedder> = if slow {
        Arc::new(SlowEmbedder(StubEmbedder::new(dim)))
    } else {
        Arc::new(StubEmbedder::new(dim))
    };
    let engine = Arc::new(MemoryEngine::new(store, persist, embedder));
    TestRig {
        engine,
        audit,
        _dir: dir,
    }
}

fn dream_rows(p: &Persist, action: &str) -> usize {
    p.list_audit(None)
        .unwrap()
        .into_iter()
        .filter(|e| e.actor == "dream" && e.action == action)
        .count()
}

fn ix(id: &str, text: &str) -> Interaction {
    Interaction {
        id: id.into(),
        session_id: "sess-dream".into(),
        tenant: String::new(),
        namespace: String::new(),
        namespace_key: String::new(),
        turns: vec![Turn {
            turn_idx: 0,
            user_message: text.into(),
            assistant_message: String::new(),
            tool_calls: vec![],
        }],
        timestamp: 0,
        owner: None,
        metadata: Default::default(),
    }
}

#[tokio::test]
async fn dream_cycle_runs_consolidation_and_audits_boundary() {
    let r = rig(8, false);
    // no extractor → every commit leaves entities_pending=1
    r.engine
        .commit_episodic_memory("sess-dream", &ix("ix1", "alpha"))
        .await
        .unwrap();
    r.engine
        .commit_episodic_memory("sess-dream", &ix("ix2", "beta"))
        .await
        .unwrap();
    assert_eq!(r.engine.pending_count().unwrap(), 2);

    let cfg = DreamConfig {
        interval_secs: 60,
        pending_threshold: 1,
    };
    assert!(
        dream_cycle_once(&r.engine, &cfg).await,
        "threshold met → ran"
    );
    assert_eq!(dream_rows(&r.audit, "trigger"), 1);
    assert_eq!(dream_rows(&r.audit, "complete"), 1);
    assert_eq!(dream_rows(&r.audit, "fail"), 0);
    // same entry as manual RPC — second cycle still legal (consolidate_lock reuse)
    assert!(dream_cycle_once(&r.engine, &cfg).await);
    assert_eq!(dream_rows(&r.audit, "trigger"), 2);
}

#[tokio::test]
async fn dream_cycle_skipped_below_threshold() {
    let r = rig(8, false);
    let cfg = DreamConfig {
        interval_secs: 60,
        pending_threshold: 5,
    };
    r.engine
        .commit_episodic_memory("sess-dream", &ix("ix1", "alpha"))
        .await
        .unwrap();
    assert_eq!(r.engine.pending_count().unwrap(), 1);
    assert!(
        !dream_cycle_once(&r.engine, &cfg).await,
        "pending 1 < 5 → skip"
    );
    assert_eq!(dream_rows(&r.audit, "trigger"), 0);
}

#[tokio::test]
async fn dream_cycle_blocked_while_request_in_flight() {
    let r = rig(8, true);
    r.engine
        .commit_episodic_memory("sess-dream", &ix("ix1", "alpha"))
        .await
        .unwrap();
    let cfg = DreamConfig {
        interval_secs: 60,
        pending_threshold: 1,
    };
    // hold a retrieve in flight (SlowEmbedder sleeps 200ms inside the engine window)
    let q = RetrieveQuery::new("alpha", 3, 1000);
    let eng = r.engine.clone();
    let inflight_task = tokio::spawn(async move { eng.retrieve_context(&q).await });
    // give the retrieve a moment to enter the in-flight window
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        r.engine.inflight_count() >= 1,
        "retrieve must be inside the in-flight window"
    );
    assert!(
        !dream_cycle_once(&r.engine, &cfg).await,
        "in-flight request → dream must not run"
    );
    assert_eq!(
        dream_rows(&r.audit, "trigger"),
        0,
        "no trigger audit while busy"
    );
    inflight_task.await.unwrap().unwrap();
    assert_eq!(
        r.engine.inflight_count(),
        0,
        "window released after retrieve"
    );
    assert!(dream_cycle_once(&r.engine, &cfg).await, "idle again → runs");
    assert_eq!(dream_rows(&r.audit, "trigger"), 1);
}

#[tokio::test]
async fn dream_and_manual_consolidate_serialize() {
    // dream cycle + manual RPC consolidation share consolidate_lock — concurrent
    // invocation must complete both without deadlock or saga interference.
    let r = rig(8, false);
    for i in 0..4 {
        r.engine
            .commit_episodic_memory("sess-dream", &ix(&format!("ix{i}"), &format!("t{i}")))
            .await
            .unwrap();
    }
    let cfg = DreamConfig {
        interval_secs: 60,
        pending_threshold: 1,
    };
    let e1 = r.engine.clone();
    let d = tokio::spawn(async move { dream_cycle_once(&e1, &cfg).await });
    let e2 = r.engine.clone();
    let m = tokio::spawn(async move { e2.consolidate_memories().await });
    let (d_res, m_res) = tokio::join!(d, m);
    assert!(d_res.unwrap(), "dream cycle completed");
    assert!(m_res.unwrap().is_ok(), "manual consolidation completed");
    assert_eq!(dream_rows(&r.audit, "trigger"), 1);
    assert_eq!(dream_rows(&r.audit, "complete"), 1);
}
