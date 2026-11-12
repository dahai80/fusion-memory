//! M2 #40: dreaming scheduler — idle-triggered background consolidation.
//!
//! Adds a trigger only; the consolidation saga kernel (fm-engine) is untouched
//! (defense line 4). Every cycle: idle check (in-flight retrieve/commit == 0) +
//! pending-entity threshold, then the same `consolidate_memories()` entry the
//! manual RPC uses — engine's consolidate_lock serializes both, so a dream can
//! never run concurrently with a manual consolidation.
//!
//! Disabled by default: `FUSION_MEMORY_DREAM_INTERVAL_SECS` unset/0 → no task.
//! Boundary events (trigger / skip / complete / fail) land in audit_log via
//! `MemoryEngine::dream_audit`.

use std::sync::Arc;
use std::time::Duration;

use fm_core::FusionMemoryEngine;
use fm_engine::MemoryEngine;
use tracing::{debug, error, info, warn};

/// Dreaming scheduler config (env-driven, default off).
#[derive(Debug, Clone, PartialEq)]
pub struct DreamConfig {
    /// Tick interval in seconds. 0 = disabled (default).
    pub interval_secs: u64,
    /// Minimum pending-entity count to trigger consolidation.
    pub pending_threshold: u64,
}

impl DreamConfig {
    /// Parse from env. Both unset → disabled (interval 0).
    pub fn from_env() -> Self {
        let interval_secs = std::env::var("FUSION_MEMORY_DREAM_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let pending_threshold = std::env::var("FUSION_MEMORY_DREAM_PENDING_THRESHOLD")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1);
        Self {
            interval_secs,
            pending_threshold,
        }
    }

    pub fn enabled(&self) -> bool {
        self.interval_secs > 0
    }
}

/// Pure trigger decision (unit-testable). Idle = no in-flight retrieve/commit.
pub fn dream_due(inflight: usize, pending: u64, cfg: &DreamConfig) -> bool {
    inflight == 0 && pending >= cfg.pending_threshold
}

/// One dream cycle: gate check + trigger + boundary audit. Returns whether
/// consolidation ran. Factored out of the loop for deterministic tests.
pub async fn dream_cycle_once(engine: &MemoryEngine, cfg: &DreamConfig) -> bool {
    let inflight = engine.inflight_count();
    let pending = match engine.pending_count() {
        Ok(n) => n,
        Err(e) => {
            warn!(error = %e, "dream: pending_count failed, cycle skipped");
            engine.dream_audit("skip", &format!("pending_count error: {e}"));
            return false;
        }
    };
    if !dream_due(inflight, pending, cfg) {
        debug!(
            inflight,
            pending,
            threshold = cfg.pending_threshold,
            "dream: gate not met"
        );
        return false;
    }
    info!(
        pending,
        "dream: idle + pending threshold met, triggering consolidation"
    );
    engine.dream_audit("trigger", &format!("pending={pending}"));
    // Same entry as manual RPC consolidate — consolidate_lock serializes both.
    match engine.consolidate_memories().await {
        Ok(rep) => {
            info!(
                dropped = rep.dropped,
                promoted = rep.promoted,
                merged = rep.merged,
                summarized = rep.summarized,
                "dream: consolidation complete"
            );
            engine.dream_audit(
                "complete",
                &format!(
                    "dropped={} promoted={} merged={} summarized={}",
                    rep.dropped, rep.promoted, rep.merged, rep.summarized
                ),
            );
            true
        }
        Err(e) => {
            error!(error = %e, "dream: consolidation failed");
            engine.dream_audit("fail", &format!("error: {e}"));
            false
        }
    }
}

/// Long-running dream loop. Spawned by `serve` only when enabled; exits on
/// shutdown signal. Tick N fires at N*interval (first tick fires immediately —
/// tokio interval semantics — harmless: gate still applies).
pub async fn dream_loop(
    engine: Arc<MemoryEngine>,
    cfg: DreamConfig,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) {
    if !cfg.enabled() {
        return;
    }
    info!(
        interval_secs = cfg.interval_secs,
        pending_threshold = cfg.pending_threshold,
        "dream scheduler started"
    );
    let mut tick = tokio::time::interval(Duration::from_secs(cfg.interval_secs));
    loop {
        tokio::select! {
            _ = shutdown.recv() => {
                info!("dream scheduler stopping (shutdown)");
                break;
            }
            _ = tick.tick() => {
                dream_cycle_once(&engine, &cfg).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dream_config_default_off() {
        // no env in test process → disabled
        let cfg = DreamConfig {
            interval_secs: 0,
            pending_threshold: 1,
        };
        assert!(!cfg.enabled());
    }

    #[test]
    fn dream_due_gate() {
        let cfg = DreamConfig {
            interval_secs: 60,
            pending_threshold: 10,
        };
        assert!(
            dream_due(0, 10, &cfg),
            "idle + pending==threshold → trigger"
        );
        assert!(dream_due(0, 50, &cfg), "pending above threshold → trigger");
        assert!(!dream_due(1, 50, &cfg), "in-flight request → skip");
        assert!(!dream_due(3, 50, &cfg), "any in-flight → skip");
        assert!(!dream_due(0, 9, &cfg), "pending below threshold → skip");
    }

    #[tokio::test]
    async fn dream_loop_disabled_returns_immediately() {
        let cfg = DreamConfig {
            interval_secs: 0,
            pending_threshold: 1,
        };
        let (tx, rx) = tokio::sync::broadcast::channel::<()>(1);
        // engine unused when disabled — dummy via open_in_memory engine would need
        // store/embedder; disabled loop returns before touching the engine, but the
        // signature needs one. Build a minimal real engine via engine_builder helpers.
        let persist = std::sync::Arc::new(fm_persist::Persist::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(fm_store::LocalStore::open(dir.path(), 8).unwrap());
        let embedder = std::sync::Arc::new(fm_embed::StubEmbedder::new(8));
        let engine = Arc::new(MemoryEngine::new(store, persist, embedder));
        tokio::time::timeout(Duration::from_millis(500), dream_loop(engine, cfg, rx))
            .await
            .expect("disabled loop must return immediately");
        drop(tx);
    }
}
