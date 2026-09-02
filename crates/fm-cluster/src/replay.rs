//! wop 重放: leader 读源 + follower 落地重放。PRD §16.4。
//!
//! commit → 反序列化 MemoryItem → 本地 re-embed + insert_vector + put_memory (§6.3 同 content 同向量)。
//! delete → tombstone (follower 本地)。
//! summarize/未知 op → 跳过 (本地 consolidate 各节点独立)。
//! §1.9: recycle/promote/merge/reextract 现 emit wop, follower 重放落地 (不再独立 consolidate 分叉)。
//! §2.7: commit wop payload 改 CommitEnvelope{item, vector} 携带 leader 向量, follower 直用免 re-embed
//!   (bge-m3 跨进程浮点非确定 → re-embed 致检索发散)。旧版纯 MemoryItem payload 仍兼容 (vector=None 回退 embed)。

use async_trait::async_trait;
use fm_core::MemoryItem;
use fm_persist::{Persist, WopEntry};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::error::{ClusterError, ClusterResult};

/// §2.7: commit wop payload 信封。携带 leader 已算向量, follower 直用免 re-embed。
/// vector=None 表示旧版 payload 或 leader 主动让 follower re-embed (降级兼容)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitEnvelope {
    pub item: MemoryItem,
    #[serde(default)]
    pub vector: Option<Vec<f32>>,
}

/// §2.7: 把 MemoryItem + 向量编码成 commit wop payload (CommitEnvelope JSON)。
pub fn encode_commit_payload(item: &MemoryItem, vector: Option<&[f32]>) -> ClusterResult<String> {
    let envelope = CommitEnvelope {
        item: item.clone(),
        vector: vector.map(|v| v.to_vec()),
    };
    Ok(serde_json::to_string(&envelope)?)
}

/// §2.7: 解码 commit wop payload。兼容新旧两格式:
/// - 新: CommitEnvelope{item, vector} → 直用 vector (None 则回退 embed)
/// - 旧: 纯 MemoryItem JSON → vector=None, follower re-embed
pub fn decode_commit_payload(raw: &str) -> ClusterResult<(MemoryItem, Option<Vec<f32>>)> {
    // 先试新信封格式
    if let Ok(env) = serde_json::from_str::<CommitEnvelope>(raw) {
        return Ok((env.item, env.vector));
    }
    // 回退旧格式 (纯 MemoryItem)
    let item: MemoryItem = serde_json::from_str(raw)
        .map_err(|e| ClusterError::Replay(format!("commit payload decode: {e}")))?;
    Ok((item, None))
}

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
    /// §1.9: 跨库状态变更落地。promote=改 tier, merge=source tombstone+记 merge_log。
    /// 默认空实现 (旧 sink 无此能力), MemoryEngine 覆写接 persist 真实操作。
    async fn promote_tier(&self, _id: &str, _tier: &str) -> ClusterResult<()> {
        Ok(())
    }
    async fn record_merge(
        &self,
        _source_id: &str,
        _target_id: &str,
        _reason: &str,
        _at: u64,
    ) -> ClusterResult<()> {
        Ok(())
    }
    /// §2.5: 同步成功通知 (follower 与 leader 追平)。MemoryEngine 覆写 → mark_stale(false)+mark_synced(now)。
    /// 返回的 u64 = 当前 ms 时间戳 (供 mark_synced), 默认 0 (旧 sink 不关心 stale-read 信号)。
    async fn on_sync_ok(&self) -> ClusterResult<()> {
        Ok(())
    }
    /// §2.5: 同步停滞通知 (leader down / 永久错误 / 退避重试中)。MemoryEngine 覆写 → mark_stale(true)。
    async fn on_sync_stale(&self) -> ClusterResult<()> {
        Ok(())
    }
}

/// 单条 wop 重放分发。返回是否已落地 (跳过的 op → false)。
/// §1.9: recycle/promote/merge/reextract 不再跳过, 各自落地。
/// §2.7: commit payload 含 vector → 直用; vector=None → re-embed 回退。
pub async fn replay_one(sink: &dyn ReplaySink, entry: &WopEntry) -> ClusterResult<bool> {
    match entry.op.as_str() {
        "commit" => {
            // §3.19: payload 解码失败 = 永久错误 (数据损坏, 重试无意义) → PermanentReplay。
            let (item, maybe_vec) = decode_commit_payload(&entry.payload).map_err(|e| {
                if e.is_permanent() {
                    e
                } else {
                    ClusterError::PermanentReplay(format!("commit payload decode: {e}"))
                }
            })?;
            let vec_id: u64 = item
                .vector_ref
                .parse()
                .map_err(|e| ClusterError::PermanentReplay(format!("vector_ref parse: {e}")))?;
            // §2.7: 优先用 payload 携带的 leader 向量 (避免 re-embed 跨节点发散);
            // 缺失 (旧版 payload / leader 降级) → fallback 本地 embed。
            let vec = match maybe_vec {
                Some(v) => v,
                None => sink.embed(&item.content).await?,
            };
            sink.insert_vector(vec_id, &vec).await?;
            sink.put_item(&item).await?;
            Ok(true)
        }
        "delete" => {
            sink.tombstone(&entry.payload).await?;
            Ok(true)
        }
        // §1.9: recycle = 物理删除的软删前奏, follower tombstone 同步 (向量由 leader 删, follower reconcile 兜底)。
        "recycle" => {
            sink.tombstone(&entry.payload).await?;
            Ok(true)
        }
        // §1.9: promote = tier 晋升 (Short→Long), payload = "id\ttier"。
        "promote" => {
            let parts: Vec<&str> = entry.payload.split('\t').collect();
            if parts.len() == 2 {
                sink.promote_tier(parts[0], parts[1]).await?;
                Ok(true)
            } else {
                warn!(
                    op = "promote",
                    seq = entry.seq,
                    "malformed promote payload, skip"
                );
                Ok(false)
            }
        }
        // §1.9: merge = source tombstone + 记 merge_log, payload = "source_id\ttarget_id\treason\tat"。
        "merge" => {
            let parts: Vec<&str> = entry.payload.split('\t').collect();
            if parts.len() == 4 {
                let at: u64 = parts[3].parse().unwrap_or(0);
                sink.tombstone(parts[0]).await?;
                sink.record_merge(parts[0], parts[1], parts[2], at).await?;
                Ok(true)
            } else {
                warn!(
                    op = "merge",
                    seq = entry.seq,
                    "malformed merge payload, skip"
                );
                Ok(false)
            }
        }
        // §1.9: reextract = 实体补抽回写, payload = MemoryItem JSON (仅 entities 变, 向量不变)。
        // follower put_item 覆写元数据 (含新 entities)。向量不动 (内容未变 → 同向量)。
        "reextract" => {
            // §3.19: payload 解码失败 = 永久错误。
            let item: MemoryItem = serde_json::from_str(&entry.payload).map_err(|e| {
                ClusterError::PermanentReplay(format!("reextract payload decode: {e}"))
            })?;
            sink.put_item(&item).await?;
            Ok(true)
        }
        other => {
            // summarize/reextract/未知 op: 本地 consolidate 各节点独立, 跳过。
            // (summarize 产生新 Semantic 记忆, follower 经其 own consolidate 生成, 跳过避免双重摘要)
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
    /// §3.19: 失败是否永久 (payload 损坏/陈旧 epoch/鉴权配置)。永久 → follower 不再退避重试该批
    /// (游标卡住, 等运维介入或数据修复), 瞬时 (mlx 429/IO busy) → 退避重试。默认 false。
    pub permanent: bool,
}

/// 批量重放一组 wop, seq 严格升序处理。返回 ReplayOutcome (不抛 Err):
/// - 单条落地失败 (mlx 429/payload 损坏/本地 sink 错) → warn + 停批 + failed=true, 已落地条目仍计入。
///   调方 (sync_once) 据此推进游标到 last_applied_seq, 失败条目下轮重拉, 不触发 failover (非 leader 宕机)。
///   §3.19: 永久失败 (permanent=true) → follower run 不再退避重试 (升级 error 日志, 等运维);
///   瞬时失败 → 退避重试 (leader/sink 可能恢复)。
/// - skipped op (summarize/未知) 推进游标 (不重拉), 本地 consolidate 各节点独立。
pub async fn replay_wops(sink: &dyn ReplaySink, entries: &[WopEntry]) -> ReplayOutcome {
    let mut applied = 0usize;
    let mut skipped = 0usize;
    let mut last_applied_seq = 0i64;
    let mut failed = false;
    let mut permanent = false;
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
                permanent = e.is_permanent();
                if permanent {
                    // §3.19: 永久失败升级 error — 重试无意义, 游标卡住等运维, 不淹没在 warn 海。
                    error!(
                        seq = entry.seq,
                        error = %e,
                        "wop replay PERMANENT fail, stop batch; cursor stuck, needs operator intervention"
                    );
                } else {
                    warn!(
                        seq = entry.seq,
                        error = %e,
                        "wop replay transient fail, stop batch; cursor stays before this seq, retry next round"
                    );
                }
                failed = true;
                break;
            }
        }
    }
    info!(
        applied,
        skipped, failed, permanent, last_applied_seq, "wop batch replayed"
    );
    ReplayOutcome {
        applied,
        skipped,
        last_applied_seq,
        failed,
        permanent,
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
            String::new(),
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
        // §3.19: payload 损坏 = 永久错误 (permanent=true), 不应重试。
        assert!(out.failed);
        assert!(out.permanent, "corrupt payload is permanent, not retryable");
        assert_eq!(out.applied, 0);
        assert_eq!(out.last_applied_seq, 0, "cursor stays before failed seq");
    }

    #[tokio::test]
    async fn replay_transient_sink_fail_not_permanent() {
        // §3.19: sink 错 (IO/busy/429) = 瞬时, permanent=false → follower 应退避重试。
        struct TransientSink;
        #[async_trait]
        impl ReplaySink for TransientSink {
            async fn embed(&self, _c: &str) -> ClusterResult<Vec<f32>> {
                Ok(vec![0.5; 4])
            }
            async fn put_item(&self, _i: &MemoryItem) -> ClusterResult<()> {
                // 瞬时: 模拟 sink busy (非永久 Replay/PermanentReplay)
                Err(ClusterError::Transport("sink busy, transient".into()))
            }
            async fn insert_vector(&self, _id: u64, _v: &[f32]) -> ClusterResult<()> {
                Ok(())
            }
            async fn tombstone(&self, _id: &str) -> ClusterResult<()> {
                Ok(())
            }
        }
        let sink = TransientSink;
        let item = sample_item();
        let payload = serde_json::to_string(&item).unwrap();
        let entries = vec![WopEntry {
            seq: 1,
            op: "commit".into(),
            payload,
            at: 100,
        }];
        let out = replay_wops(&sink, &entries).await;
        assert!(out.failed);
        assert!(
            !out.permanent,
            "transport/sink error is transient, should retry"
        );
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
