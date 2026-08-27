//! wop 重放: leader 读源 + follower 落地重放。PRD §16.4。
//!
//! commit → 反序列化 MemoryItem → 本地 re-embed + insert_vector + put_memory (§6.3 同 content 同向量)。
//! delete → tombstone (follower 本地)。
//! summarize/未知 op → 跳过 (本地 consolidate 各节点独立)。

use async_trait::async_trait;
use fm_core::MemoryItem;
use fm_persist::{Persist, WopEntry};
use tracing::{info, warn};

use crate::error::{ClusterError, ClusterResult};

/// leader 端 wop 源 (由 Arc<Persist> 实现)。
pub trait WopSource: Send + Sync {
    fn list_wop_since(&self, since_seq: i64, limit: usize) -> ClusterResult<Vec<WopEntry>>;
    fn last_wop_seq(&self) -> ClusterResult<i64>;
}

// Persist 已有 list_wop_since/last_wop_seq, impl 让 leader 直接复用 engine.persist()。
// impl 在 Persist 本身 (非 Arc<Persist>) 以支持 Arc<Persist> → Arc<dyn WopSource> trait-object 强转。
impl WopSource for Persist {
    fn list_wop_since(&self, since_seq: i64, limit: usize) -> ClusterResult<Vec<WopEntry>> {
        Persist::list_wop_since(self, since_seq, limit).map_err(ClusterError::from)
    }
    fn last_wop_seq(&self) -> ClusterResult<i64> {
        Persist::last_wop_seq(self).map_err(ClusterError::from)
    }
}

/// follower 端重放落地 (由 MemoryEngine 经适配实现)。
#[async_trait]
pub trait ReplaySink: Send + Sync {
    async fn embed(&self, content: &str) -> ClusterResult<Vec<f32>>;
    async fn put_item(&self, item: &MemoryItem) -> ClusterResult<()>;
    async fn insert_vector(&self, vec_id: u64, vec: &[f32]) -> ClusterResult<()>;
    async fn tombstone(&self, id: &str) -> ClusterResult<()>;
}

/// 单条 wop 重放分发。返回是否已落地 (跳过的 op → false)。
pub async fn replay_one(sink: &dyn ReplaySink, entry: &WopEntry) -> ClusterResult<bool> {
    match entry.op.as_str() {
        "commit" => {
            let item: MemoryItem = serde_json::from_str(&entry.payload)
                .map_err(|e| ClusterError::Replay(format!("commit payload decode: {e}")))?;
            let vec = sink.embed(&item.content).await?;
            let vec_id: u64 = item
                .vector_ref
                .parse()
                .map_err(|e| ClusterError::Replay(format!("vector_ref parse: {e}")))?;
            sink.insert_vector(vec_id, &vec).await?;
            sink.put_item(&item).await?;
            Ok(true)
        }
        "delete" => {
            sink.tombstone(&entry.payload).await?;
            Ok(true)
        }
        other => {
            // summarize / merge / 未知 op: 本地 consolidate 各节点独立, 跳过。
            warn!(op = other, seq = entry.seq, "wop op skipped on replay");
            Ok(false)
        }
    }
}

/// 重放结果。H2 修正: 携带 last_applied_seq 让 follower 游标推进到本批已落地最大 seq,
/// 失败条目不计入 → 下轮 since_seq 重拉失败条目, 已落地条目不重复重放 (幂等前提: stub idempotent)。
/// failed=true 表示本批有条目落地失败 (已 warn + 停批), 非 leader 宕机, 调方不触发 failover。
#[derive(Debug, Clone, Default)]
pub struct ReplayOutcome {
    pub applied: usize,
    pub skipped: usize,
    /// 本批已落地/已跳过条目的最大 seq。失败条目不含 → 游标卡在失败条目前, 下轮重拉。
    pub last_applied_seq: i64,
    pub failed: bool,
}

/// 批量重放一组 wop, seq 严格升序处理。返回 ReplayOutcome (不抛 Err):
/// - 单条落地失败 (mlx 429/payload 损坏/本地 sink 错) → warn + 停批 + failed=true, 已落地条目仍计入。
///   调方 (sync_once) 据此推进游标到 last_applied_seq, 失败条目下轮重拉, 不触发 failover (非 leader 宕机)。
/// - skipped op (summarize/未知) 推进游标 (不重拉), 本地 consolidate 各节点独立。
pub async fn replay_wops(sink: &dyn ReplaySink, entries: &[WopEntry]) -> ReplayOutcome {
    let mut applied = 0usize;
    let mut skipped = 0usize;
    let mut last_applied_seq = 0i64;
    let mut failed = false;
    for entry in entries {
        match replay_one(sink, entry).await {
            Ok(true) => {
                applied += 1;
                last_applied_seq = last_applied_seq.max(entry.seq);
            }
            Ok(false) => {
                skipped += 1;
                last_applied_seq = last_applied_seq.max(entry.seq);
            }
            Err(e) => {
                warn!(
                    seq = entry.seq,
                    error = %e,
                    "wop replay failed, stop batch; cursor stays before this seq, retry next round"
                );
                failed = true;
                break;
            }
        }
    }
    info!(
        applied,
        skipped, failed, last_applied_seq, "wop batch replayed"
    );
    ReplayOutcome {
        applied,
        skipped,
        last_applied_seq,
        failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_core::MemoryType;
    use std::sync::Mutex;

    struct FakeSink {
        puts: Mutex<Vec<MemoryItem>>,
        tombstones: Mutex<Vec<String>>,
        vectors: Mutex<Vec<(u64, Vec<f32>)>>,
    }

    impl FakeSink {
        fn new() -> Self {
            Self {
                puts: Mutex::new(Vec::new()),
                tombstones: Mutex::new(Vec::new()),
                vectors: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ReplaySink for FakeSink {
        async fn embed(&self, _content: &str) -> ClusterResult<Vec<f32>> {
            Ok(vec![0.5; 4])
        }
        async fn put_item(&self, item: &MemoryItem) -> ClusterResult<()> {
            self.puts.lock().unwrap().push(item.clone());
            Ok(())
        }
        async fn insert_vector(&self, vec_id: u64, vec: &[f32]) -> ClusterResult<()> {
            self.vectors.lock().unwrap().push((vec_id, vec.to_vec()));
            Ok(())
        }
        async fn tombstone(&self, id: &str) -> ClusterResult<()> {
            self.tombstones.lock().unwrap().push(id.to_string());
            Ok(())
        }
    }

    fn sample_item() -> MemoryItem {
        let mut item = MemoryItem::new_turn_skeleton(
            "01H".into(),
            "int-1".into(),
            0,
            "sess-1".into(),
            MemoryType::Episodic,
            "hello world".into(),
            100,
        );
        item.vector_ref = "42".into();
        item.entities_pending = true;
        item
    }

    #[tokio::test]
    async fn replay_commit_and_delete() {
        let sink = FakeSink::new();
        let item = sample_item();
        let payload = serde_json::to_string(&item).unwrap();
        let entries = vec![
            WopEntry {
                seq: 1,
                op: "commit".into(),
                payload,
                at: 100,
            },
            WopEntry {
                seq: 2,
                op: "delete".into(),
                payload: "01H".into(),
                at: 200,
            },
        ];
        let out = replay_wops(&sink, &entries).await;
        assert_eq!(out.applied, 2);
        assert_eq!(out.skipped, 0);
        assert!(!out.failed);
        assert_eq!(out.last_applied_seq, 2);
        assert_eq!(sink.puts.lock().unwrap().len(), 1);
        assert_eq!(sink.vectors.lock().unwrap()[0].0, 42);
        let tombs: Vec<String> = sink.tombstones.lock().unwrap().clone();
        assert_eq!(tombs, vec!["01H".to_string()]);
    }

    #[tokio::test]
    async fn replay_skips_summarize() {
        let sink = FakeSink::new();
        let entries = vec![WopEntry {
            seq: 1,
            op: "summarize".into(),
            payload: "id".into(),
            at: 100,
        }];
        let out = replay_wops(&sink, &entries).await;
        assert_eq!(out.applied, 0);
        assert_eq!(out.skipped, 1);
        assert!(!out.failed);
        // skipped op 推进游标 (不重拉)
        assert_eq!(out.last_applied_seq, 1);
        assert!(sink.puts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn replay_bad_commit_payload_errors() {
        let sink = FakeSink::new();
        let entries = vec![WopEntry {
            seq: 1,
            op: "commit".into(),
            payload: "not json".into(),
            at: 100,
        }];
        let out = replay_wops(&sink, &entries).await;
        // H2: 单条落地失败 → failed=true, applied=0, last_applied_seq 卡在失败条目前 (0)
        assert!(out.failed);
        assert_eq!(out.applied, 0);
        assert_eq!(out.last_applied_seq, 0, "cursor stays before failed seq");
    }

    #[tokio::test]
    async fn replay_empty_batch() {
        let sink = FakeSink::new();
        let out = replay_wops(&sink, &[]).await;
        assert_eq!(out.applied, 0);
        assert_eq!(out.skipped, 0);
        assert!(!out.failed);
        assert_eq!(out.last_applied_seq, 0);
    }

    #[tokio::test]
    async fn replay_partial_batch_cursor_advances_past_applied() {
        // H2: seq=1 ok, seq=2 fail, seq=3 未处理。
        // applied=1, failed=true, last_applied_seq=1 (推进过已落地, 未越过失败条目)。
        struct FailOnSecond;
        #[async_trait]
        impl ReplaySink for FailOnSecond {
            async fn embed(&self, _c: &str) -> ClusterResult<Vec<f32>> {
                Ok(vec![0.5; 4])
            }
            async fn put_item(&self, _i: &MemoryItem) -> ClusterResult<()> {
                Ok(())
            }
            async fn insert_vector(&self, _id: u64, _v: &[f32]) -> ClusterResult<()> {
                Err(ClusterError::Replay("simulated sink fail".into()))
            }
            async fn tombstone(&self, _id: &str) -> ClusterResult<()> {
                Ok(())
            }
        }
        let sink = FailOnSecond;
        let item = sample_item();
        let payload = serde_json::to_string(&item).unwrap();
        let entries = vec![
            WopEntry {
                seq: 1,
                op: "delete".into(),
                payload: "a".into(),
                at: 100,
            },
            WopEntry {
                seq: 2,
                op: "commit".into(),
                payload,
                at: 200,
            },
            WopEntry {
                seq: 3,
                op: "delete".into(),
                payload: "b".into(),
                at: 300,
            },
        ];
        let out = replay_wops(&sink, &entries).await;
        assert!(out.failed);
        assert_eq!(out.applied, 1, "seq=1 delete ok");
        assert_eq!(
            out.last_applied_seq, 1,
            "cursor at seq=1, not past failed seq=2"
        );
    }
}
