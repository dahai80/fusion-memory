//! B-2 自动 failover e2e 集成测试 (v1.0.0)。
//!
//! 证明 README/MEMORY 所载 "leader 宕机 → follower 自动竞选 → 新 leader 续写" 真实落地
//! (旧 claim 仅 election.rs 单元 + cluster_e2e 手动 promote, 未覆盖 orbit 全链路)。
//!
//! 驱动生产入口 spawn_cluster(role=Follower): 真实 MemoryEngine + 真实 in-process TCP。
//! 拓扑: 3 节点集群, candidate=self_id 1。peer 0/2 起 serve_votes 真 vote listener (授权投票);
//! candidate 1 起 Follower::run (leader_addr=死端口) → LeaderDown → follower_orbit campaign
//! → request_vote 打 peer listener → quorum (2) → 胜出 → 写 epoch++ + role=Leader → orbit 返 Ok。
//! 断言: orbit task Ok; read_epoch_file 递增; role 文件 = leader;
//! detect_role_with_home 读 role 文件得 Leader (env 已清, 走文件分支)。
//!
//! env 全局 → ENV_LOCK 串行 (与 cluster.rs / config.rs 测试同锁模式)。
//! 100% 离线: 仅 127.0.0.1 环回, 无外网。无 mock, 真 TCP 真选举。

use std::sync::{Arc, Mutex, OnceLock};

use fm_cluster::{
    detect_role_with_home, read_epoch_file, serve_votes, Election, ElectionConfig, LogSeqProvider,
    NodeRole,
};
use fm_embed::StubEmbedder;
use fm_engine::MemoryEngine;
use fm_persist::Persist;
use fm_server::cluster::spawn_cluster;
use fm_store::LocalStore;
use tempfile::TempDir;
use tokio::task::JoinSet;

// env 全局, 并行测试串扰 → 互斥锁串行化 (复用 cluster.rs 同模式)。
static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
fn lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

fn make_engine(dir: &std::path::Path) -> Arc<MemoryEngine> {
    let store = Arc::new(LocalStore::open(dir, 4).unwrap());
    let persist = Arc::new(Persist::open_in_memory().unwrap());
    let embedder: Arc<dyn fm_embed::Embedder> = Arc::new(StubEmbedder::new(4));
    Arc::new(MemoryEngine::new(store, persist, embedder))
}

// 空闲端口探测: 绑 127.0.0.1:0 取 OS 分配端口后立即关 (复用 leader bind 模式)。
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

// 供 peer vote listener 的 Election 读日志序号 (空库, 返 0; 判据 4 candidate_seq>=own 满足)。
struct ZeroSeq;
#[async_trait::async_trait]
impl LogSeqProvider for ZeroSeq {
    async fn last_log_seq(&self) -> Result<i64, fm_cluster::ClusterError> {
        Ok(0)
    }
}

#[tokio::test]
async fn leader_down_auto_elects_new_leader() {
    // 清 role env: 断言 detect_role_with_home 须走文件分支 (非 env 覆盖)。
    std::env::remove_var("FUSION_MEMORY_ROLE");

    // 拓扑: 3 节点环回。candidate=self_id 1, peers 0/2 起 vote listener。
    let p0 = free_port();
    let p1 = free_port();
    let p2 = free_port();
    let dead_leader = format!("127.0.0.1:{}", free_port());
    let nodes = vec![
        format!("127.0.0.1:{p0}"),
        format!("127.0.0.1:{p1}"),
        format!("127.0.0.1:{p2}"),
    ];
    let token = "test-failover-token".to_string();

    // peer ElectionConfig: 同 nodes/token, self_id 各异 (from_env 读全局 NODE_ID=1, 不可复用)。
    let peer_ec = |id: usize| ElectionConfig {
        nodes: nodes.clone(),
        self_id: id,
        heartbeat_secs: 1,
        heartbeat_fails: 2,
        cluster_token: Some(token.clone()),
    };

    // peer vote listener 先起 (campaign 来请求时须就绪)。不读 env, 无需持锁。
    let seq: Arc<dyn LogSeqProvider> = Arc::new(ZeroSeq);
    let mut peer_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    for id in [0usize, 2] {
        let listener = tokio::net::TcpListener::bind(&nodes[id]).await.unwrap();
        let election = Arc::new(Election::new(peer_ec(id)));
        let seq = seq.clone();
        peer_handles.push(tokio::spawn(async move {
            let _ = serve_votes(listener, election, seq).await;
        }));
    }

    // candidate (self_id 1) env: NODES + NODE_ID + TOKEN + 死端口 leader + 快心跳。
    // env 全局 → 锁仅护 env-set + spawn_cluster (同步读 env), 毕即放, 不跨 await。
    let candidate_dir = TempDir::new().unwrap();
    let engine = make_engine(candidate_dir.path());
    let mut set: JoinSet<Result<(), String>> = JoinSet::new();
    {
        let _g = lock();
        std::env::set_var("FUSION_MEMORY_CLUSTER_NODES", nodes.join(","));
        std::env::set_var("FUSION_MEMORY_CLUSTER_NODE_ID", "1");
        std::env::set_var("FUSION_MEMORY_CLUSTER_TOKEN", &token);
        std::env::set_var("FUSION_MEMORY_LEADER", &dead_leader);
        std::env::set_var("FUSION_MEMORY_HEARTBEAT_SECS", "1");
        std::env::set_var("FUSION_MEMORY_HEARTBEAT_FAILS", "2");
        spawn_cluster(engine, NodeRole::Follower, candidate_dir.path(), &mut set);
    }

    // orbit: LeaderDown (2 fails × 1s ≈ 2s) → campaign → quorum → 写 epoch/role → Ok。
    // vote listener task 永驻, 仅 orbit task 返 Ok → join_next 取到即胜出。
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(20), set.join_next()).await;
    set.shutdown().await;
    for h in peer_handles {
        h.abort();
    }

    let joined = outcome
        .expect("orbit task did not complete within 20s (election failed?)")
        .expect("join_next none");
    let task_res = joined.expect("orbit task panicked");
    assert!(
        task_res.is_ok(),
        "orbit task should return Ok on election win, got: {task_res:?}"
    );

    // 断言 1: epoch 递增 (0 → 1, fencing 防陈旧 leader)。
    let epoch = read_epoch_file(candidate_dir.path());
    assert_eq!(
        epoch, 1,
        "epoch must increment after winning election (fencing)"
    );
    // 断言 2: role 文件 = leader (supervisor 重启后 detect_role 读此成 Leader)。
    let role_str = std::fs::read_to_string(candidate_dir.path().join("role"))
        .expect("role file written")
        .trim()
        .to_string();
    assert_eq!(role_str, "leader", "role file must be leader after win");
    // 断言 3: detect_role_with_home 走文件分支 (env 已清) → Leader。
    assert_eq!(
        detect_role_with_home(Some(candidate_dir.path())),
        NodeRole::Leader,
        "detect_role_with_home must read Leader from role file"
    );

    // 清 env (恢复, 避免污染后续测试)。
    std::env::remove_var("FUSION_MEMORY_CLUSTER_NODES");
    std::env::remove_var("FUSION_MEMORY_CLUSTER_NODE_ID");
    std::env::remove_var("FUSION_MEMORY_CLUSTER_TOKEN");
    std::env::remove_var("FUSION_MEMORY_LEADER");
    std::env::remove_var("FUSION_MEMORY_HEARTBEAT_SECS");
    std::env::remove_var("FUSION_MEMORY_HEARTBEAT_FAILS");
}
