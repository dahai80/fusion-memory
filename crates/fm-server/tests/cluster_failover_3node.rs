//! M9 #51: cluster auto-failover e2e — real 3-node topology over real TCP.
//!
//! Versus the v1.0.1 election_failover e2e (dead leader address from the
//! start), this test proves the full plan-§6 v1.9.0 acceptance chain:
//! 1. Live leader serves WOP sync; a commit on the leader replicates to both
//!    follower engines (cross-node sync verification).
//! 2. The live leader is killed (its JoinSet shut down) — followers detect
//!    LeaderDown via heartbeat failures, one campaigns, wins quorum (2/3),
//!    writes role=Leader + epoch++ and its orbit task returns Ok.
//! 3. Restart simulation: the promoted node serves as leader on the new epoch
//!    (epoch fencing), and a fresh follower expecting the new epoch syncs a
//!    post-failover commit from it.
//!
//! 100% offline: 127.0.0.1 loopback only, real TCP, real engines, no mocks.
//! ElectionConfig is injected per node via spawn_cluster_with (v1.9.0) —
//! NODE_ID is process-global env and cannot express three self_ids at once.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use fm_cluster::{
    detect_role_with_home, read_epoch_file, ElectionConfig, Follower, Leader, NodeRole, SyncConfig,
};
use fm_core::interaction::{Interaction, Turn};
use fm_core::FusionMemoryEngine;
use fm_embed::StubEmbedder;
use fm_engine::MemoryEngine;
use fm_persist::Persist;
use fm_server::cluster::spawn_cluster_with;
use fm_store::LocalStore;
use tempfile::TempDir;
use tokio::task::JoinSet;

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

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

fn interaction(id: &str, text: &str) -> Interaction {
    Interaction {
        id: id.into(),
        session_id: "sess-1".into(),
        tenant: String::new(),
        namespace: String::new(),
        namespace_key: String::new(),
        turns: vec![Turn {
            turn_idx: 0,
            user_message: text.into(),
            assistant_message: String::new(),
            tool_calls: vec![],
        }],
        timestamp: 1000,
        owner: None,
        metadata: serde_json::json!({}),
    }
}

async fn wait_for_memory(eng: &MemoryEngine, id: &str, secs: u64) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < deadline {
        if eng.get_memory(id).await.unwrap().is_some() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

#[tokio::test]
async fn three_node_sync_kill_leader_auto_promotion() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();
    std::env::remove_var("FUSION_MEMORY_ROLE");

    let token = "test-3node-token".to_string();
    let dirs: Vec<TempDir> = (0..3).map(|_| TempDir::new().unwrap()).collect();
    let engines: Vec<Arc<MemoryEngine>> = dirs.iter().map(|d| make_engine(d.path())).collect();
    let leader_port = free_port();
    let leader_addr = format!("127.0.0.1:{leader_port}");

    // Election topology: 3 nodes, fast heartbeat (1s x 2 fails = ~2s detection).
    let nodes = vec![
        format!("127.0.0.1:{}", free_port()),
        format!("127.0.0.1:{}", free_port()),
        format!("127.0.0.1:{}", free_port()),
    ];
    let ec = |id: usize| ElectionConfig {
        nodes: nodes.clone(),
        self_id: id,
        heartbeat_secs: 1,
        heartbeat_fails: 2,
        cluster_token: Some(token.clone()),
    };

    // Shared sync env: leader addr + fast heartbeat (read at spawn time only).
    // Per-node self_id comes from the injected ElectionConfig, not env.
    let mut set_leader: JoinSet<Result<(), String>> = JoinSet::new();
    let mut set_f1: JoinSet<Result<(), String>> = JoinSet::new();
    let mut set_f2: JoinSet<Result<(), String>> = JoinSet::new();
    {
        let _g = lock();
        std::env::set_var("FUSION_MEMORY_CLUSTER_TOKEN", &token);
        std::env::set_var("FUSION_MEMORY_LEADER", &leader_addr);
        std::env::set_var("FUSION_MEMORY_HEARTBEAT_SECS", "1");
        std::env::set_var("FUSION_MEMORY_HEARTBEAT_FAILS", "2");
        std::env::set_var("FUSION_MEMORY_SYNC_PORT", leader_port.to_string());
        // Node 0: leader (sync serve + its own vote listener).
        spawn_cluster_with(
            engines[0].clone(),
            NodeRole::Leader,
            dirs[0].path(),
            &mut set_leader,
            Some(ec(0)),
        );
        // Nodes 1/2: followers (sync orbit + vote listeners).
        spawn_cluster_with(
            engines[1].clone(),
            NodeRole::Follower,
            dirs[1].path(),
            &mut set_f1,
            Some(ec(1)),
        );
        spawn_cluster_with(
            engines[2].clone(),
            NodeRole::Follower,
            dirs[2].path(),
            &mut set_f2,
            Some(ec(2)),
        );
    }

    // 1. Cross-node sync: commit on the leader, both followers must receive it.
    let ids = engines[0]
        .commit_episodic_memory("sess-1", &interaction("ix-3node", "zebra anchor memory"))
        .await
        .unwrap();
    let mem_id = ids[0].clone();
    assert!(
        wait_for_memory(&engines[1], mem_id.as_str(), 15).await,
        "follower 1 must replicate the leader commit"
    );
    assert!(
        wait_for_memory(&engines[2], mem_id.as_str(), 15).await,
        "follower 2 must replicate the leader commit"
    );

    // 2. Kill the live leader. Followers detect LeaderDown (~2s), campaign,
    //    quorum 2/3 (dead node 0 cannot vote) → one wins, writes role+epoch.
    set_leader.shutdown().await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut winner: Option<(usize, ())> = None;
    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            res = set_f1.join_next(), if !set_f1.is_empty() => {
                eprintln!("probe f1 join: {res:?}");
                if let Some(Ok(Ok(()))) = res {
                    winner = Some((1, ()));
                    break;
                }
            }
            res = set_f2.join_next(), if !set_f2.is_empty() => {
                eprintln!("probe f2 join: {res:?}");
                if let Some(Ok(Ok(()))) = res {
                    winner = Some((2, ()));
                    break;
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
    // Exactly one promotion window: stop the loser immediately so it cannot
    // run a second campaign past the assertion point.
    let (winner_id, loser_set_done) = match winner {
        Some((1, ())) => {
            set_f2.shutdown().await;
            (1, true)
        }
        Some((2, ())) => {
            set_f1.shutdown().await;
            (2, true)
        }
        None => panic!("no follower won promotion within 30s"),
        _ => unreachable!("winner id is 1 or 2 only"),
    };
    let _ = loser_set_done;

    // Promotion artifacts: role=Leader via file branch (env unset) + epoch 0 -> 1.
    assert_eq!(
        detect_role_with_home(Some(dirs[winner_id].path())),
        NodeRole::Leader,
        "winner must have role=Leader written"
    );
    assert_eq!(
        read_epoch_file(dirs[winner_id].path()),
        1,
        "winner epoch must be incremented (fencing)"
    );

    // 3. Restart simulation: promoted node serves as leader on epoch 1; a
    //    fresh follower expecting epoch 1 syncs a post-failover commit.
    let new_port = free_port();
    let source: Arc<dyn fm_cluster::WopSource> = engines[winner_id].persist().clone();
    let mut set_new_leader: JoinSet<Result<(), String>> = JoinSet::new();
    let token_restart = token.clone();
    set_new_leader.spawn(async move {
        let leader = Arc::new(
            Leader::new(source, new_port)
                .with_token(Some(token_restart))
                .with_epoch(1),
        );
        leader
            .serve()
            .await
            .map_err(|e| format!("promoted leader: {e}"))
    });

    // Commit post-failover data on the promoted leader.
    let ids2 = engines[winner_id]
        .commit_episodic_memory(
            "sess-1",
            &interaction("ix-post", "post failover falcon memory"),
        )
        .await
        .unwrap();

    // Fresh follower engine, expects epoch 1 (stale epoch-0 leaders rejected).
    let catchup_dir = TempDir::new().unwrap();
    let catchup = make_engine(catchup_dir.path());
    let cfg = SyncConfig {
        leader_addr: format!("127.0.0.1:{new_port}"),
        heartbeat_secs: 1,
        heartbeat_fails: 2,
        fetch_limit: 100,
        cluster_token: Some(token.clone()),
        epoch: 1,
    };
    let mut follower = Follower::new(cfg, catchup.clone(), catchup.last_wop_seq().unwrap_or(0));
    // Sync rounds: the post-failover commit must land. (The pre-failover row
    // lives in the winner's data rows but not its wop log — replayed rows are
    // applied directly without a wop append — so a fresh catch-up follower
    // receives post-promotion writes only. Known lean-election limitation.)
    let mut got_new = false;
    for _ in 0..10 {
        let _ = follower.sync_once().await;
        if catchup
            .get_memory(ids2[0].as_str())
            .await
            .unwrap()
            .is_some()
        {
            got_new = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    set_new_leader.shutdown().await;
    assert!(
        got_new,
        "catch-up follower must receive the post-failover commit from the promoted leader"
    );
}
