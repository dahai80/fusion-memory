//! 集群拓扑装配: 按 NodeRole 起 leader / follower 同步 task。PRD §16。
//!
//! standalone → 无 task。leader → Leader::serve (源=engine.persist())。
//! follower + FUSION_MEMORY_LEADER → Follower::run (落地=engine, local_last_seq=engine.last_wop_seq)。

use std::sync::Arc;

use fm_cluster::{ClusterConfig, Follower, Leader, NodeRole, ReplaySink, SyncConfig, WopSource};
use fm_engine::MemoryEngine;
use tokio::task::JoinSet;
use tracing::{info, warn};

/// 按 role 装配集群同步 task 到 set。无配置 (standalone/无 leader) → 不 spawn。
/// role 由调用方传入 (serve 经 detect_role 读 env), 使测试可注入确定 role 免 env 竞争。
pub fn spawn_cluster(
    engine: Arc<MemoryEngine>,
    role: NodeRole,
    set: &mut JoinSet<Result<(), String>>,
) {
    info!(role = %role, "cluster role detected");
    match role {
        NodeRole::Leader => {
            let port = ClusterConfig::default().sync_port;
            let source: Arc<dyn WopSource> = engine.persist().clone();
            let leader = Arc::new(Leader::new(source, port));
            set.spawn(async move {
                leader
                    .serve()
                    .await
                    .map_err(|e| format!("cluster leader: {e}"))
            });
        }
        NodeRole::Follower => match SyncConfig::from_env() {
            Some(cfg) => {
                let local_last_seq = engine
                    .last_wop_seq()
                    .map_err(|e| format!("cluster follower last_seq: {e}"))
                    .unwrap_or(0);
                let sink: Arc<dyn ReplaySink> = engine.clone();
                let follower = Follower::new(cfg, sink, local_last_seq);
                set.spawn(async move {
                    follower
                        .run()
                        .await
                        .map_err(|e| format!("cluster follower: {e}"))
                });
            }
            None => {
                warn!("role=follower 但 FUSION_MEMORY_LEADER 未配, 跳过集群同步");
            }
        },
        NodeRole::Standalone => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_embed::StubEmbedder;
    use fm_persist::Persist;
    use fm_store::StoreStub;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    // Leader::new 用 ClusterConfig::default().sync_port; 测试需 FUSION_MEMORY_SYNC_PORT=0 占空闲端口。
    // SyncConfig::from_env 读 FUSION_MEMORY_LEADER。env 全局, 互斥锁串行化改写。
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn make_engine() -> (Arc<MemoryEngine>, TempDir) {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(StoreStub::open(dir.path(), 4).unwrap());
        let persist = Arc::new(Persist::open_in_memory().unwrap());
        let embedder: Arc<dyn fm_embed::Embedder> = Arc::new(StubEmbedder::new(4));
        (Arc::new(MemoryEngine::new(store, persist, embedder)), dir)
    }

    #[tokio::test]
    async fn standalone_spawns_nothing() {
        // role 注入 → 无 env 依赖, 无竞争。
        let (engine, _dir) = make_engine();
        let mut set: JoinSet<Result<(), String>> = JoinSet::new();
        spawn_cluster(engine, NodeRole::Standalone, &mut set);
        assert!(set.is_empty());
    }

    #[tokio::test]
    async fn follower_without_leader_env_spawns_nothing() {
        // role=follower 但 FUSION_MEMORY_LEADER 未配 → from_env None → 不 spawn。
        let _g = lock();
        std::env::remove_var("FUSION_MEMORY_LEADER");
        let (engine, _dir) = make_engine();
        let mut set: JoinSet<Result<(), String>> = JoinSet::new();
        spawn_cluster(engine, NodeRole::Follower, &mut set);
        assert!(set.is_empty());
    }

    #[tokio::test]
    async fn follower_with_leader_env_spawns_one_task() {
        // role=follower + FUSION_MEMORY_LEADER 配 → Follower::run 起, set.len 1。
        let mut set: JoinSet<Result<(), String>> = JoinSet::new();
        {
            let _g = lock();
            std::env::set_var("FUSION_MEMORY_LEADER", "127.0.0.1:65535");
            let (engine, _dir) = make_engine();
            spawn_cluster(engine, NodeRole::Follower, &mut set);
            assert_eq!(set.len(), 1);
            std::env::remove_var("FUSION_MEMORY_LEADER");
        }
        set.shutdown().await;
    }

    #[tokio::test]
    async fn leader_spawns_one_task() {
        // role=leader → Leader::serve 起 (sync_port=0 占空闲), set.len 1。锁在块尾 drop 后再 await。
        let mut set: JoinSet<Result<(), String>> = JoinSet::new();
        {
            let _g = lock();
            std::env::set_var("FUSION_MEMORY_SYNC_PORT", "0");
            let (engine, _dir) = make_engine();
            spawn_cluster(engine, NodeRole::Leader, &mut set);
            assert_eq!(set.len(), 1);
            std::env::remove_var("FUSION_MEMORY_SYNC_PORT");
        }
        set.shutdown().await;
    }
}
