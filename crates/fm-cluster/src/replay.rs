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

/// 批量重放一组 wop, 返回 (已落地条数, 跳过条数)。seq 严格升序处理。
pub async fn replay_wops(
    sink: &dyn ReplaySink,
    entries: &[WopEntry],
) -> ClusterResult<(usize, usize)> {
    let mut applied = 0usize;
    let mut skipped = 0usize;
    for entry in entries {
        match replay_one(sink, entry).await {
            Ok(true) => applied += 1,
            Ok(false) => skipped += 1,
            Err(e) => {
                warn!(seq = entry.seq, error = %e, "wop replay failed, stopping batch");
                return Err(e);
            }
        }
    }
    info!(applied, skipped, "wop batch replayed");
    Ok((applied, skipped))
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
        let (applied, skipped) = replay_wops(&sink, &entries).await.unwrap();
        assert_eq!(applied, 2);
        assert_eq!(skipped, 0);
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
        let (applied, skipped) = replay_wops(&sink, &entries).await.unwrap();
        assert_eq!(applied, 0);
        assert_eq!(skipped, 1);
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
        let err = replay_wops(&sink, &entries).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn replay_empty_batch() {
        let sink = FakeSink::new();
        let (applied, skipped) = replay_wops(&sink, &[]).await.unwrap();
        assert_eq!(applied, 0);
        assert_eq!(skipped, 0);
    }
}
