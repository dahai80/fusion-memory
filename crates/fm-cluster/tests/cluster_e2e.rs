//! M6 集群端到端集成验收。PRD §16。
//!
//! 场景:
//! 1. leader 写 commit wop → follower sync_once → 落地 item (read-local 一致)。
//! 2. 增量同步: 二次 sync_once 仅取新 wop, seq 推进。
//! 3. leader 停 → follower sync 连败 → LeaderDown → write_role_file(promote) → 新 leader 续写。
//!
//! 真实 Persist (文件) 做 WopSource, CountingSink 记落地。in-process TCP (127.0.0.1:0)。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fm_cluster::{
    write_role_file, ClusterError, Follower, Leader, NodeRole, ReplaySink, SyncConfig,
};
use fm_core::MemoryItem;
use fm_persist::Persist;
use tempfile::TempDir;

// 真实 Persist 源: append_wop 落 SQLite, list_wop_since/last_wop_seq 供 leader 读。
fn open_source() -> (Arc<Persist>, TempDir) {
    let dir = TempDir::new().unwrap();
    let p = Arc::new(Persist::open(dir.path().join("wop.db")).unwrap());
    (p, dir)
}

// 记落地: put/tombstone/vector 计数 + 内容快照。
struct RecordSink {
    puts: Mutex<Vec<String>>,
    tombs: Mutex<Vec<String>>,
    vectors: Mutex<Vec<u64>>,
}
impl RecordSink {
    fn new() -> Self {
        Self {
            puts: Mutex::new(Vec::new()),
            tombs: Mutex::new(Vec::new()),
            vectors: Mutex::new(Vec::new()),
        }
    }
}
#[async_trait]
impl ReplaySink for RecordSink {
    async fn embed(&self, _c: &str) -> Result<Vec<f32>, ClusterError> {
        Ok(vec![0.7; 4])
    }
    async fn put_item(&self, item: &MemoryItem) -> Result<(), ClusterError> {
        self.puts.lock().unwrap().push(item.id.clone());
        Ok(())
    }
    async fn insert_vector(&self, id: u64, _v: &[f32]) -> Result<(), ClusterError> {
        self.vectors.lock().unwrap().push(id);
        Ok(())
    }
    async fn tombstone(&self, id: &str) -> Result<(), ClusterError> {
        self.tombs.lock().unwrap().push(id.to_string());
        Ok(())
    }
}

// 造一条 commit wop (serialized MemoryItem) 写入 leader Persist。
fn append_commit(source: &Persist, id: &str, seq_vec: u64) {
    let mut item = MemoryItem::new_turn_skeleton(
        id.into(),
        format!("ix-{id}"),
        0,
        "sess-1".into(),
        fm_core::MemoryType::Episodic,
        format!("content {id}"),
        100,
    );
    item.vector_ref = seq_vec.to_string();
    item.entities_pending = true;
    let payload = serde_json::to_string(&item).unwrap();
    source.append_wop("commit", &payload, 100).unwrap();
}

#[tokio::test]
async fn e2e_leader_commit_follower_catchup() {
    let (source, _dir) = open_source();
    append_commit(&source, "m-1", 11);
    let leader = Arc::new(Leader::new(source.clone(), 0));
    let (listener, port) = leader.bind().await.unwrap();
    let leader_task = tokio::spawn(Arc::clone(&leader).serve_listener(listener));

    let sink = Arc::new(RecordSink::new());
    let mut follower = Follower::new(
        SyncConfig {
            leader_addr: format!("127.0.0.1:{port}"),
            heartbeat_secs: 1,
            heartbeat_fails: 3,
            fetch_limit: 64,
            cluster_token: None,
        },
        sink.clone(),
        0,
    );
    let (seq, applied, _failed) = follower.sync_once().await.unwrap();
    assert_eq!(seq, 1);
    assert_eq!(applied, 1);
    assert_eq!(sink.puts.lock().unwrap().clone(), vec!["m-1".to_string()]);
    assert_eq!(sink.vectors.lock().unwrap().clone(), vec![11]);
    leader_task.abort();
}

#[tokio::test]
async fn e2e_incremental_sync_advances_seq() {
    let (source, _dir) = open_source();
    append_commit(&source, "a", 1);
    let leader = Arc::new(Leader::new(source.clone(), 0));
    let (listener, port) = leader.bind().await.unwrap();
    let leader_task = tokio::spawn(Arc::clone(&leader).serve_listener(listener));

    let sink = Arc::new(RecordSink::new());
    let mut follower = Follower::new(
        SyncConfig {
            leader_addr: format!("127.0.0.1:{port}"),
            heartbeat_secs: 1,
            heartbeat_fails: 3,
            fetch_limit: 64,
            cluster_token: None,
        },
        sink.clone(),
        0,
    );
    // 第一轮: 取 a
    let (seq1, app1, _f1) = follower.sync_once().await.unwrap();
    assert_eq!((seq1, app1), (1, 1));

    // leader 追加第二条 (delete) + 第三条 (commit b)
    source.append_wop("delete", "a", 200).unwrap();
    append_commit(&source, "b", 2);

    // 第二轮: 增量取 seq 2,3 (delete a + commit b)
    let (seq2, app2, _f2) = follower.sync_once().await.unwrap();
    assert_eq!(seq2, 3);
    assert_eq!(app2, 2);
    let tombs = sink.tombs.lock().unwrap().clone();
    assert_eq!(tombs, vec!["a".to_string()]);
    assert_eq!(
        sink.puts.lock().unwrap().clone(),
        vec!["a".to_string(), "b".to_string()]
    );
    leader_task.abort();
}

#[tokio::test]
async fn e2e_leader_down_then_promote_new_leader() {
    // 旧 leader: 写一条后 abort (模拟宕机)。
    let (source_a, _dir_a) = open_source();
    append_commit(&source_a, "old-1", 7);
    let leader_a = Arc::new(Leader::new(source_a.clone(), 0));
    let (listener_a, port_a) = leader_a.bind().await.unwrap();
    let leader_task_a = tokio::spawn(Arc::clone(&leader_a).serve_listener(listener_a));

    let sink = Arc::new(RecordSink::new());
    let follower = Follower::new(
        SyncConfig {
            leader_addr: format!("127.0.0.1:{port_a}"),
            heartbeat_secs: 0,
            heartbeat_fails: 2,
            fetch_limit: 64,
            cluster_token: None,
        },
        sink.clone(),
        0,
    );
    // run() 先追上 old-1, leader 宕机后连败 → LeaderDown。5s 超时兜底。
    let run_task = tokio::spawn(follower.run());
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    leader_task_a.abort();
    drop(leader_a);
    let res = tokio::time::timeout(std::time::Duration::from_secs(5), run_task).await;
    let down = matches!(res, Ok(Ok(Err(ClusterError::LeaderDown(_)))));
    assert!(down, "follower run() 应返回 LeaderDown");
    // old-1 已追上 (read-local 一致)
    assert_eq!(sink.puts.lock().unwrap().clone(), vec!["old-1".to_string()]);

    // 手动 failover: 本节点提升为 leader (写 role 文件, 模拟 fm cluster promote)
    let promote_dir = TempDir::new().unwrap();
    let role_path = write_role_file(promote_dir.path(), NodeRole::Leader).unwrap();
    assert_eq!(std::fs::read_to_string(&role_path).unwrap(), "leader");

    // 新 leader 续写 (原 follower 数据 + 新 wop)
    let (source_b, _dir_b) = open_source();
    append_commit(&source_b, "new-1", 8);
    let leader_b = Arc::new(Leader::new(source_b.clone(), 0));
    let (listener_b, port_b) = leader_b.bind().await.unwrap();
    let leader_task_b = tokio::spawn(Arc::clone(&leader_b).serve_listener(listener_b));

    // 另一个 follower 接新 leader, 追上 new-1
    let sink2 = Arc::new(RecordSink::new());
    let mut follower2 = Follower::new(
        SyncConfig {
            leader_addr: format!("127.0.0.1:{port_b}"),
            heartbeat_secs: 1,
            heartbeat_fails: 3,
            fetch_limit: 64,
            cluster_token: None,
        },
        sink2.clone(),
        0,
    );
    let (seq_new, app_new, _fnew) = follower2.sync_once().await.unwrap();
    assert_eq!(seq_new, 1);
    assert_eq!(app_new, 1);
    assert_eq!(
        sink2.puts.lock().unwrap().clone(),
        vec!["new-1".to_string()]
    );
    leader_task_b.abort();
}
