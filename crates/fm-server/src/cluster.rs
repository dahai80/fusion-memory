//! 集群拓扑装配: 按 NodeRole 起 leader / follower 同步 task。PRD §16。
//!
//! standalone → 无 task。leader → Leader::serve (源=engine.persist())。
//! follower + FUSION_MEMORY_LEADER → Follower::run (落地=engine, local_last_seq=engine.last_wop_seq)。
//! §1.8: leader/follower epoch 经 ClusterConfig::with_home(data_dir) 读 (env 优先, 次读 home/epoch 文件)。
//! §2.5: follower 同步 ok/stale 经 ReplaySink::on_sync_ok/on_sync_stale 注入 engine.mark_stale/mark_synced。

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use fm_cluster::{
    serve_votes, ClusterConfig, ClusterError, ClusterResult, Election, ElectionConfig, Follower,
    Leader, LogSeqProvider, NodeRole, ReplaySink, SyncConfig, WopSource,
};
use fm_engine::MemoryEngine;
use tokio::task::JoinSet;
use tracing::{error, info, warn};

/// v1.0.0 B-2: MemoryEngine → LogSeqProvider 适配。竞选判"日志足够新"用 engine.last_wop_seq。
struct EngineSeqProvider {
    engine: Arc<MemoryEngine>,
}

#[async_trait]
impl LogSeqProvider for EngineSeqProvider {
    async fn last_log_seq(&self) -> ClusterResult<i64> {
        self.engine
            .last_wop_seq()
            .map_err(|e| ClusterError::Transport(format!("engine last_wop_seq: {e}")))
    }
}

/// 按 role 装配集群同步 task 到 set。无配置 (standalone/无 leader) → 不 spawn。
/// role 由调用方传入 (serve 经 detect_role 读 env), 使测试可注入确定 role 免 env 竞争。
/// §1.8: data_dir 供 ClusterConfig::with_home 读 epoch 文件 (fm cluster promote 落地)。
/// v1.0.0 B-2: 选举配置 (FUSION_MEMORY_CLUSTER_NODES) 存在 → follower 起 vote listener + 竞选 orbit,
/// LeaderDown → 自动竞选, 胜出 → epoch++ + role file 写 Leader, 退出让 supervisor 重启成 leader。
pub fn spawn_cluster(
    engine: Arc<MemoryEngine>,
    role: NodeRole,
    data_dir: &Path,
    set: &mut JoinSet<Result<(), String>>,
) {
    info!(role = %role, "cluster role detected");
    // v1.0.0 B-2: 选举配置 (集群 ≥2 节点)。存在 → 起各节点 vote listener (leader + follower 都监听)。
    let election_cfg = match ElectionConfig::from_env() {
        Ok(Some(c)) => {
            info!(
                nodes = c.nodes.len(),
                self_id = c.self_id,
                "election enabled (B-2 auto-failover)"
            );
            Some(c)
        }
        Ok(None) => None,
        Err(e) => {
            error!(error = %e, "election config invalid, election disabled");
            None
        }
    };
    match role {
        NodeRole::Leader => {
            let cfg = ClusterConfig::with_home(data_dir);
            let port = cfg.sync_port;
            let source: Arc<dyn WopSource> = engine.persist().clone();
            // §1.8: with_epoch (fencing) + with_bind_addr (跨机) + with_allow_no_token (单机测试)。
            let leader = Arc::new(
                Leader::new(source, port)
                    .with_token(cfg.cluster_token)
                    .with_epoch(cfg.epoch)
                    .with_bind_addr(cfg.bind_addr.clone())
                    .with_allow_no_token(cfg.allow_no_token),
            );
            set.spawn(async move {
                leader
                    .serve()
                    .await
                    .map_err(|e| format!("cluster leader: {e}"))
            });
            // v1.0.0 B-2: leader 也起 vote listener (作为投票方, 参与新 leader 选举)。
            if let Some(ec) = election_cfg {
                let election = Arc::new(Election::new(ec.clone()));
                spawn_vote_listener_with(engine.clone(), ec, election, set);
            }
        }
        NodeRole::Follower => match SyncConfig::from_env() {
            Some(cfg) => {
                let local_last_seq = engine
                    .last_wop_seq()
                    .map_err(|e| format!("cluster follower last_seq: {e}"))
                    .unwrap_or(0);
                let sink: Arc<dyn ReplaySink> = engine.clone();
                // v1.0.0 B-2: 选举配置存在 → follower run 包竞选 orbit (LeaderDown → 竞选)。
                // 否则原行为: run 直接退出 (LeaderDown 返 Err → 上层重试/退出)。
                if let Some(ec) = election_cfg {
                    let seq_provider: Arc<dyn LogSeqProvider> = Arc::new(EngineSeqProvider {
                        engine: engine.clone(),
                    });
                    let election = Arc::new(Election::new(ec.clone()));
                    // vote listener (follower 监听本节点, 供候选请求投票)。
                    spawn_vote_listener_with(engine.clone(), ec.clone(), election.clone(), set);
                    let data_dir = data_dir.to_path_buf();
                    set.spawn(async move {
                        follower_orbit(cfg, sink, local_last_seq, election, seq_provider, data_dir)
                            .await
                            .map_err(|e| format!("cluster follower (election orbit): {e}"))
                    });
                } else {
                    let follower = Follower::new(cfg, sink, local_last_seq);
                    set.spawn(async move {
                        follower
                            .run()
                            .await
                            .map_err(|e| format!("cluster follower: {e}"))
                    });
                }
            }
            None => {
                warn!("role=follower 但 FUSION_MEMORY_LEADER 未配, 跳过集群同步");
            }
        },
        NodeRole::Standalone => {}
    }
}

/// v1.0.0 B-2: vote listener (复用 election handle)。leader + follower 各一份。
fn spawn_vote_listener_with(
    engine: Arc<MemoryEngine>,
    ec: ElectionConfig,
    election: Arc<Election>,
    set: &mut JoinSet<Result<(), String>>,
) {
    let my_addr = ec.nodes[ec.self_id].clone();
    let seq_provider: Arc<dyn LogSeqProvider> = Arc::new(EngineSeqProvider { engine });
    set.spawn(async move {
        let listener = tokio::net::TcpListener::bind(&my_addr)
            .await
            .map_err(|e| format!("vote listener bind {my_addr}: {e}"))?;
        info!(%my_addr, "vote listener starting");
        serve_votes(listener, election, seq_provider)
            .await
            .map_err(|e| format!("vote listener {my_addr}: {e}"))
    });
}

/// v1.0.0 B-2: follower 同步 + 竞选 orbit。run() LeaderDown → campaign → 胜出转 leader (写 role+epoch)。
/// 胜出: 写 role=Leader + epoch++ 到 data_dir, 返 Ok 让 supervisor (serve JoinSet) 重启为新 leader。
/// 败/未决: 退避后重建 Follower 再 run (新 leader 已选, 续同步, 再次 LeaderDown 则再竞选)。
/// 注: Follower::run 取 self (ownership), 故每轮 orbit 重建 Follower (cfg/sink 可 Clone/Arc 复用)。
async fn follower_orbit(
    cfg: SyncConfig,
    sink: Arc<dyn ReplaySink>,
    initial_seq: i64,
    election: Arc<Election>,
    seq_provider: Arc<dyn LogSeqProvider>,
    data_dir: std::path::PathBuf,
) -> Result<(), String> {
    let mut local_seq = initial_seq;
    loop {
        let follower = Follower::new(cfg.clone(), sink.clone(), local_seq);
        match follower.run().await {
            Ok(()) => return Ok(()),
            Err(e) => {
                // LeaderDown = 候选竞选触发点。其他错误 (StaleLeader/Auth) = 永久, 不竞选 (重试无意义)。
                if !matches!(e, ClusterError::LeaderDown(_)) {
                    return Err(format!("follower run permanent error: {e}"));
                }
                warn!(error = %e, "leader down, initiating election campaign");
                let won = election
                    .campaign(seq_provider.clone())
                    .await
                    .map_err(|e| format!("campaign: {e}"))?;
                if won {
                    // 胜出: epoch++ (fencing 防 stale leader) + 写 role=Leader (重启成 leader)。
                    let epoch = fm_cluster::read_epoch_file(&data_dir);
                    let new_epoch = epoch + 1;
                    fm_cluster::write_epoch_file(&data_dir, new_epoch)
                        .map_err(|e| format!("write epoch: {e}"))?;
                    fm_cluster::write_role_file(&data_dir, NodeRole::Leader)
                        .map_err(|e| format!("write role: {e}"))?;
                    info!(
                        new_epoch,
                        "won election, promoted to leader, restart to serve"
                    );
                    // 返 Ok: orbit 退出, serve JoinSet 视为该 task 正常退出。
                    // 下次启动 detect_role_with_home 读 role=Leader → 起 Leader::serve。
                    // 注: 当前进程不就地转 leader (避免与运行中 vote listener 端口冲突), 需 supervisor 重启。
                    return Ok(());
                }
                // 败: 退避后重建 follower 再 run (新 leader 已选, 续同步)。
                warn!("lost election, backoff then re-follow");
                tokio::time::sleep(election.cfg().lease()).await;
                // 重置 local_seq 为当前 engine seq (竞选期数据可能变化), 下轮 run 重新对齐。
                local_seq = seq_provider.last_log_seq().await.unwrap_or(local_seq);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_embed::StubEmbedder;
    use fm_persist::Persist;
    use fm_store::LocalStore;
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
        let store = Arc::new(LocalStore::open(dir.path(), 4).unwrap());
        let persist = Arc::new(Persist::open_in_memory().unwrap());
        let embedder: Arc<dyn fm_embed::Embedder> = Arc::new(StubEmbedder::new(4));
        (Arc::new(MemoryEngine::new(store, persist, embedder)), dir)
    }

    #[tokio::test]
    async fn standalone_spawns_nothing() {
        // role 注入 → 无 env 依赖, 无竞争。
        let (engine, dir) = make_engine();
        let mut set: JoinSet<Result<(), String>> = JoinSet::new();
        spawn_cluster(engine, NodeRole::Standalone, dir.path(), &mut set);
        assert!(set.is_empty());
    }

    #[tokio::test]
    async fn follower_without_leader_env_spawns_nothing() {
        // role=follower 但 FUSION_MEMORY_LEADER 未配 → from_env None → 不 spawn。
        let _g = lock();
        std::env::remove_var("FUSION_MEMORY_LEADER");
        let (engine, dir) = make_engine();
        let mut set: JoinSet<Result<(), String>> = JoinSet::new();
        spawn_cluster(engine, NodeRole::Follower, dir.path(), &mut set);
        assert!(set.is_empty());
    }

    #[tokio::test]
    async fn follower_with_leader_env_spawns_one_task() {
        // role=follower + FUSION_MEMORY_LEADER 配 → Follower::run 起, set.len 1。
        let mut set: JoinSet<Result<(), String>> = JoinSet::new();
        {
            let _g = lock();
            std::env::set_var("FUSION_MEMORY_LEADER", "127.0.0.1:65535");
            let (engine, dir) = make_engine();
            spawn_cluster(engine, NodeRole::Follower, dir.path(), &mut set);
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
            let (engine, dir) = make_engine();
            spawn_cluster(engine, NodeRole::Leader, dir.path(), &mut set);
            assert_eq!(set.len(), 1);
            std::env::remove_var("FUSION_MEMORY_SYNC_PORT");
        }
        set.shutdown().await;
    }
}
