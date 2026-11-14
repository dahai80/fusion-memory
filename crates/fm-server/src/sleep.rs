//! P10 #60: sleep-time curation scheduler — idle-triggered offline curation pass.
//!
//! Adds a trigger only; the curation kernel (fm-engine `sleep_curation`: consolidate
//! precompute + admission-gate replay) is untouched from the outside. Every tick:
//! idle check (in-flight retrieve/commit == 0), then the same `sleep_curation()`
//! entry the manual RPC uses — consolidate_lock serializes with manual consolidations.
//!
//! Disabled by default: `FUSION_MEMORY_SLEEP_INTERVAL_SECS` unset/0 → no task.
//! Boundary events land in audit_log via `MemoryEngine::dream_audit`.

use std::sync::Arc;
use std::time::Duration;

use fm_core::FusionMemoryEngine;
use fm_engine::MemoryEngine;
use tracing::{debug, error, info};

/// Sleep scheduler config (env-driven, default off).
#[derive(Debug, Clone, PartialEq)]
pub struct SleepConfig {
    /// Tick interval in seconds. 0 = disabled (default).
    pub interval_secs: u64,
}

impl SleepConfig {
    pub fn from_env() -> Self {
        let interval_secs = std::env::var("FUSION_MEMORY_SLEEP_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        Self { interval_secs }
    }

    pub fn enabled(&self) -> bool {
        self.interval_secs > 0
    }
}

/// Pure trigger decision (unit-testable). Idle = no in-flight retrieve/commit.
pub fn sleep_due(inflight: usize) -> bool {
    inflight == 0
}

/// One sleep cycle: idle gate + trigger + boundary audit. Factored out for tests.
pub async fn sleep_cycle_once(engine: &MemoryEngine) -> bool {
    let inflight = engine.inflight_count();
    if !sleep_due(inflight) {
        debug!(inflight, "sleep: request in flight, cycle skipped");
        return false;
    }
    engine.dream_audit("sleep-trigger", "idle tick");
    match engine.sleep_curation().await {
        Ok(rep) => {
            info!(
                downweighted = rep.replay_downweighted,
                demoted = rep.playbook_demoted,
                duration_ms = rep.duration_ms,
                "sleep: curation pass complete"
            );
            true
        }
        Err(e) => {
            error!(error = %e, "sleep: curation pass failed");
            engine.dream_audit("sleep-fail", &format!("error: {e}"));
            false
        }
    }
}

/// Long-running sleep loop. Spawned by `serve` only when enabled; exits on shutdown.
pub async fn sleep_loop(
    engine: Arc<MemoryEngine>,
    cfg: SleepConfig,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) {
    if !cfg.enabled() {
        return;
    }
    info!(
        interval_secs = cfg.interval_secs,
        "sleep-time curation scheduler started"
    );
    let mut tick = tokio::time::interval(Duration::from_secs(cfg.interval_secs));
    loop {
        tokio::select! {
            _ = shutdown.recv() => {
                info!("sleep scheduler stopping (shutdown)");
                break;
            }
            _ = tick.tick() => {
                sleep_cycle_once(&engine).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_config_default_off() {
        let cfg = SleepConfig { interval_secs: 0 };
        assert!(!cfg.enabled());
    }

    #[test]
    fn sleep_due_idle_gate() {
        assert!(sleep_due(0), "idle → trigger");
        assert!(!sleep_due(1), "any in-flight request → skip");
        assert!(!sleep_due(3), "in-flight requests → skip");
    }

    #[tokio::test]
    async fn sleep_loop_disabled_returns_immediately() {
        let cfg = SleepConfig { interval_secs: 0 };
        let (tx, rx) = tokio::sync::broadcast::channel::<()>(1);
        let persist = std::sync::Arc::new(fm_persist::Persist::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(fm_store::LocalStore::open(dir.path(), 8).unwrap());
        let embedder = std::sync::Arc::new(fm_embed::StubEmbedder::new(8));
        let engine = Arc::new(MemoryEngine::new(store, persist, embedder));
        tokio::time::timeout(Duration::from_millis(500), sleep_loop(engine, cfg, rx))
            .await
            .expect("disabled loop must return immediately");
        drop(tx);
    }

    // P10 #60 acceptance: a scheduled curation run updates assets with no interactive
    // request in flight; results carry admission-gate markers (idempotent second pass).
    #[tokio::test]
    async fn sleep_cycle_updates_assets_offline_and_idempotent() {
        use fm_core::{MemoryItem, MemoryType};

        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(fm_store::LocalStore::open(dir.path(), 8).unwrap());
        let persist = std::sync::Arc::new(fm_persist::Persist::open_in_memory().unwrap());
        let embedder: std::sync::Arc<dyn fm_embed::Embedder> =
            std::sync::Arc::new(fm_embed::StubEmbedder::new(8));
        // Setup via raw handles (deterministic created_timestamp ordering: m0 older).
        let base: u64 = 1_800_000_000_000; // ms — engine decay math is ms-based
        for (idx, content) in [
            "deploy the service with the retry policy",
            "deploy the service with the retry policy",
            "unrelated note about the invoice pipeline",
        ]
        .into_iter()
        .enumerate()
        {
            let mut item = MemoryItem::new_turn_skeleton(
                format!("m{idx}"),
                format!("ix{idx}"),
                0,
                format!("s{idx}"),
                String::new(),
                MemoryType::Episodic,
                content.to_string(),
                base + idx as u64,
            );
            item.entities_pending = false;
            let vec = embedder.embed(content).await.unwrap();
            use fm_store::FusionStoreEngine as _;
            store.insert_vector(100 + idx as u64, &vec).unwrap();
            item.vector_ref = (100 + idx).to_string();
            persist.put_memory(&item).unwrap();
        }
        let engine = Arc::new(MemoryEngine::new(
            store.clone(),
            persist.clone(),
            embedder.clone(),
        ));
        // No interactive request in flight: direct scheduled-style trigger.
        assert!(sleep_cycle_once(&engine).await, "first pass runs");
        // Superseded (older) duplicate carries the admission-gate marker + halved weight.
        let old = engine.get_memory("m0").await.unwrap().unwrap();
        assert_eq!(
            old.admission.as_ref().map(|r| r.reason.as_str()),
            Some("replay-duplicate")
        );
        assert!(old.weight < MemoryType::Episodic.initial_weight());
        let newest = engine.get_memory("m1").await.unwrap().unwrap();
        assert!(
            newest.admission.is_none(),
            "canonical (newest) copy untouched"
        );
        // Idempotent: second pass is a no-op (marker skip), warm-start watermark advanced.
        let r2 = engine.sleep_curation().await.unwrap();
        assert_eq!(
            r2.replay_checked, 2,
            "marked row skipped, others re-checked"
        );
        assert_eq!(r2.replay_downweighted, 0, "no double downweight");
        assert_eq!(r2.consolidated.dropped, 0);
        assert_eq!(r2.consolidated.merged, 0);
    }

    // P10 #60 playbook curation: an older playbook copy superseded by a newer
    // near-duplicate is demoted Pinned→Normal and admission-marked (batch pass).
    #[tokio::test]
    async fn sleep_curation_demotes_superseded_playbook_copy() {
        use fm_core::{MemoryItem, MemoryType, SafetyPersistence};

        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(fm_store::LocalStore::open(dir.path(), 8).unwrap());
        let persist = std::sync::Arc::new(fm_persist::Persist::open_in_memory().unwrap());
        let embedder: std::sync::Arc<dyn fm_embed::Embedder> =
            std::sync::Arc::new(fm_embed::StubEmbedder::new(8));
        let base: u64 = 1_800_000_000_000;
        for (idx, content) in [
            "runbook: rotate the api key every quarter",
            "runbook: rotate the api key every quarter",
        ]
        .into_iter()
        .enumerate()
        {
            let mut item = MemoryItem::new_turn_skeleton(
                format!("p{idx}"),
                format!("ix{idx}"),
                0,
                format!("s{idx}"),
                String::new(),
                MemoryType::Procedural,
                content.to_string(),
                base + idx as u64,
            )
            .with_namespace(fm_core::namespace::NS_PLAYBOOK, "publisher-a")
            .with_safety(
                fm_core::SafetyScope::Global,
                fm_core::SafetyAuthority::Team,
                SafetyPersistence::Pinned,
            );
            item.entities_pending = false;
            let vec = embedder.embed(content).await.unwrap();
            use fm_store::FusionStoreEngine as _;
            store.insert_vector(200 + idx as u64, &vec).unwrap();
            item.vector_ref = (200 + idx).to_string();
            persist.put_memory(&item).unwrap();
        }
        let engine = Arc::new(MemoryEngine::new(store, persist, embedder));
        let rep = engine.sleep_curation().await.unwrap();
        assert_eq!(rep.playbook_demoted, 1, "older playbook copy demoted");
        let old = engine.get_memory("p0").await.unwrap().unwrap();
        assert_eq!(
            old.admission.as_ref().map(|r| r.reason.as_str()),
            Some("playbook-superseded")
        );
        assert_eq!(old.persistence, SafetyPersistence::Normal, "unpinned");
        let newest = engine.get_memory("p1").await.unwrap().unwrap();
        assert_eq!(
            newest.persistence,
            SafetyPersistence::Pinned,
            "canonical kept pinned"
        );
    }
}
