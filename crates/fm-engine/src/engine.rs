//! MemoryEngine: FusionMemoryEngine 实现。PRD §6, §11.1, §11.4, B3/C5。
//!
//! commit: turn 级拆分 → embed → 同步写 persist(WAL)+store(向量) → 异步抽实体补 entities。
//! retrieve: query→embed→store KNN→persist 聚合 interaction_id → 融合评分排序。
//! consolidate: W(t)<θ_drop 回收 tombstone; Short→Long 晋升; entities_pending 批量重抽。
//! Embedder/EntityExtractor 注入 (测试用 stub, 生产用 mlx)。

use std::sync::Arc;

use async_trait::async_trait;
use fm_core::{
    ConsolidationReport, ContextBlock, FormattedContext, Interaction, MemoryId, MemoryItem,
    MemoryTier, RetrieveQuery, Turn,
};
use fm_embed::{vector_id_from_ulid, Embedder};
use fm_persist::Persist;
use fm_store::{FusionStoreEngine, StoreStub};
use tracing::{debug, info, warn};

use crate::entity_extract::{EntityExtractor, ExtractResult};
use crate::scoring;

const AGG_MAX_TURNS: usize = 20;

pub struct MemoryEngine {
    store: Arc<StoreStub>,
    persist: Arc<Persist>,
    embedder: Arc<dyn Embedder>,
    extractor: Option<Arc<dyn EntityExtractor>>,
}

impl MemoryEngine {
    pub fn new(store: Arc<StoreStub>, persist: Arc<Persist>, embedder: Arc<dyn Embedder>) -> Self {
        Self {
            store,
            persist,
            embedder,
            extractor: None,
        }
    }

    /// 注入实体抽取器 (生产用 MlxEntityExtractor)。不注入则 entities 永远 pending。
    pub fn with_extractor(mut self, extractor: Arc<dyn EntityExtractor>) -> Self {
        self.extractor = Some(extractor);
        self
    }

    pub fn persist(&self) -> &Arc<Persist> {
        &self.persist
    }

    pub fn store(&self) -> &Arc<StoreStub> {
        &self.store
    }

    pub fn embedder(&self) -> &Arc<dyn Embedder> {
        &self.embedder
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    fn new_ulid() -> String {
        ulid::Ulid::new().to_string()
    }

    fn turn_content(turn: &Turn) -> String {
        let mut s = String::new();
        s.push_str(&turn.user_message);
        s.push('\n');
        s.push_str(&turn.assistant_message);
        for tc in &turn.tool_calls {
            s.push('\n');
            s.push_str(&tc.name);
            s.push(':');
            s.push_str(&tc.result_summary);
        }
        s
    }

    /// 异步抽实体并回写。失败 → entities_pending 保持 true (C5: content+vector 已存)。
    async fn extract_and_attach(&self, memory_id: &str, content: &str) {
        let Some(extractor) = &self.extractor else {
            debug!(id = memory_id, "no extractor, entities_pending stays true");
            return;
        };
        let ExtractResult { entities, success } = extractor.extract(content).await;
        if let Some(mut item) = self.persist.get_memory(memory_id).unwrap_or(None) {
            item.entities = entities.clone();
            // 成功抽出 (即使空数组) → pending=false; 失败 → pending=true 待重抽
            item.entities_pending = !success;
            if let Err(e) = self.persist.put_memory(&item) {
                warn!(id = memory_id, error = %e, "回写 entities 失败");
                return;
            }
            // 抽出的实体写 entity 表 + memory_entity (put_memory 已处理 entity 表)
            // 同名异 type 对齐: rule-priority (fm-graph)。M2 简版: 直接用抽取的 id。
            debug!(
                id = memory_id,
                n = entities.len(),
                success,
                "entities attached"
            );
        }
    }
}

#[async_trait]
impl fm_core::FusionMemoryEngine for MemoryEngine {
    async fn commit_episodic_memory(
        &self,
        session_id: &str,
        interaction: &Interaction,
    ) -> fm_core::MemoryResult<Vec<MemoryId>> {
        let mut ids = Vec::with_capacity(interaction.turns.len());
        let now = Self::now_ms();
        for turn in &interaction.turns {
            let id = Self::new_ulid();
            let content = Self::turn_content(turn);
            let mut item = MemoryItem::new_turn_skeleton(
                id.clone(),
                interaction.id.clone(),
                turn.turn_idx,
                session_id.to_string(),
                fm_core::MemoryType::Episodic,
                content.clone(),
                now,
            );
            let vec_id = vector_id_from_ulid(&id);
            let vec = self
                .embedder
                .embed(&content)
                .await
                .map_err(|e| e.to_memory())?;
            self.store.insert_vector(vec_id, &vec)?;
            item.vector_ref = vec_id.to_string();
            // entities_pending=true: 实体待异步抽 (C5: content+vector 先存)
            item.entities_pending = true;
            self.persist.put_memory(&item).map_err(|e| e.to_memory())?;
            self.persist
                .append_wop("commit", &id, now)
                .map_err(|e| e.to_memory())?;
            ids.push(MemoryId(id.clone()));
            // 异步抽实体回写 (不阻塞 commit 返回; 此处同步 await 保证可测)
            self.extract_and_attach(&id, &content).await;
        }
        info!(interaction = %interaction.id, turns = interaction.turns.len(), "commit done");
        Ok(ids)
    }

    async fn retrieve_context(
        &self,
        query: &RetrieveQuery,
    ) -> fm_core::MemoryResult<FormattedContext> {
        let now = Self::now_ms();
        let qvec = self
            .embedder
            .embed(&query.text)
            .await
            .map_err(|e| e.to_memory())?;
        let knn = self.store.search_knn(&qvec, query.top_k)?;
        debug!(hits = knn.len(), "knn done");
        let all = self.persist.list_all().map_err(|e| e.to_memory())?;
        let mut by_vec_ref: std::collections::HashMap<u64, MemoryItem> =
            std::collections::HashMap::new();
        for m in all {
            if let Ok(vr) = m.vector_ref.parse::<u64>() {
                by_vec_ref.insert(vr, m);
            }
        }
        // 按 interaction 聚合 + 融合评分
        let mut groups: std::collections::BTreeMap<String, Vec<MemoryItem>> =
            std::collections::BTreeMap::new();
        let mut best_score: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();
        for (vec_id, sim) in &knn {
            if let Some(m) = by_vec_ref.get(vec_id) {
                if let Some(filter) = &query.tier_filter {
                    if !filter.contains(&m.tier) {
                        continue;
                    }
                }
                if let Some(sess) = &query.session_id {
                    if &m.session_id != sess {
                        continue;
                    }
                }
                // 融合评分: cosine + W(t) + graph_affinity
                let query_entity_ids: Vec<String> = Vec::new(); // query 无实体 (M2: 仅文本检索)
                let score =
                    scoring::score_candidate(&self.persist, *sim as f64, m, &query_entity_ids, now)
                        .map_err(|e| fm_core::MemoryError::Store(e.to_string()))?;
                let entry = best_score.entry(m.interaction_id.clone()).or_insert(0.0);
                if score > *entry {
                    *entry = score;
                }
                groups
                    .entry(m.interaction_id.clone())
                    .or_default()
                    .push(m.clone());
            }
        }
        // 组装 blocks: 命中 interaction_id → list_by_interaction 补全全部 turns
        let mut blocks: Vec<(ContextBlock, f64)> = Vec::new();
        for ix_id in groups.keys() {
            let score = best_score.get(ix_id).copied().unwrap_or(0.0);
            let mut sorted = self
                .persist
                .list_by_interaction(ix_id)
                .map_err(|e| e.to_memory())?;
            sorted.sort_by_key(|m| m.turn_idx);
            sorted.truncate(AGG_MAX_TURNS);
            let turns: Vec<Turn> = sorted
                .iter()
                .map(|m| Turn {
                    turn_idx: m.turn_idx,
                    user_message: m.content.clone(),
                    assistant_message: String::new(),
                    tool_calls: Vec::new(),
                })
                .collect();
            let turns_text = sorted
                .iter()
                .map(|m| m.content.clone())
                .collect::<Vec<_>>()
                .join("\n---\n");
            let mem_type = sorted
                .first()
                .map(|m| m.memory_type)
                .unwrap_or(fm_core::MemoryType::Episodic);
            let source_entities = sorted
                .iter()
                .flat_map(|m| m.entities.iter().map(|e| e.id.clone()))
                .collect::<Vec<_>>();
            blocks.push((
                ContextBlock {
                    interaction_id: ix_id.clone(),
                    turns,
                    memory_type: mem_type,
                    turns_text,
                    score,
                    source_entities,
                },
                score,
            ));
        }
        blocks.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        // token 预算截断 (粗算: 1 char ≈ 0.3 token; M2 换 Qwen tokenizer)
        let mut total_tokens = 0usize;
        let mut kept = Vec::new();
        for (block, _) in blocks {
            let block_tokens = (block.turns_text.len() as f64 * 0.3) as usize;
            if total_tokens + block_tokens > query.token_budget && !kept.is_empty() {
                break;
            }
            total_tokens += block_tokens;
            kept.push(block);
            // 命中即 touch access (召回计数 +1, 供 consolidate 晋升判定)
            for t in &sorted_groups_turn_ids(&groups, &kept) {
                let _ = self.persist.touch_access(t, now);
            }
        }
        Ok(FormattedContext {
            blocks: kept,
            total_tokens,
        })
    }

    async fn consolidate_memories(&self) -> fm_core::MemoryResult<ConsolidationReport> {
        let start = Self::now_ms();
        let now = start;
        let all = self.persist.list_all().map_err(|e| e.to_memory())?;
        let mut dropped = 0usize;
        let mut promoted = 0usize;
        let mut reextracted = 0usize;
        for m in &all {
            // 回收: W(t) < θ_drop → tombstone
            if scoring::should_recycle(m, now) {
                if let Ok(vr) = m.vector_ref.parse::<u64>() {
                    let _ = self.store.delete_vector(vr);
                }
                self.persist
                    .tombstone_memory(&m.id)
                    .map_err(|e| e.to_memory())?;
                dropped += 1;
                continue;
            }
            // 晋升: Short → Long
            if m.tier == MemoryTier::Short && scoring::should_promote(m, now) {
                let mut promoted_item = m.clone();
                promoted_item.tier = MemoryTier::Long;
                self.persist
                    .put_memory(&promoted_item)
                    .map_err(|e| e.to_memory())?;
                promoted += 1;
            }
            // 重抽: entities_pending 且有 extractor
            if m.entities_pending {
                if let Some(extractor) = &self.extractor {
                    let ExtractResult { entities, success } = extractor.extract(&m.content).await;
                    if success {
                        let mut item = m.clone();
                        item.entities = entities;
                        item.entities_pending = false;
                        self.persist.put_memory(&item).map_err(|e| e.to_memory())?;
                        reextracted += 1;
                    }
                }
            }
        }
        let report = ConsolidationReport {
            elapsed_ms: Self::now_ms().saturating_sub(start),
            dropped,
            promoted,
            reextracted,
            ..Default::default()
        };
        self.persist
            .record_consolidation(&report, start)
            .map_err(|e| e.to_memory())?;
        info!(dropped, promoted, reextracted, "consolidate done");
        Ok(report)
    }

    async fn get_memory(&self, id: &str) -> fm_core::MemoryResult<Option<MemoryItem>> {
        self.persist.get_memory(id).map_err(|e| e.to_memory())
    }

    async fn delete_memory(&self, id: &str) -> fm_core::MemoryResult<()> {
        if let Some(item) = self.persist.get_memory(id).map_err(|e| e.to_memory())? {
            if let Ok(vec_id) = item.vector_ref.parse::<u64>() {
                self.store.delete_vector(vec_id)?;
            }
        }
        self.persist
            .tombstone_memory(id)
            .map_err(|e| e.to_memory())?;
        info!(id, "memory tombstoned");
        Ok(())
    }

    async fn audit_memory_access(
        &self,
        entity_ids: &[String],
    ) -> fm_core::MemoryResult<Vec<MemoryItem>> {
        let all = self.persist.list_all().map_err(|e| e.to_memory())?;
        let wanted: std::collections::HashSet<&str> =
            entity_ids.iter().map(|s| s.as_str()).collect();
        let out: Vec<MemoryItem> = all
            .into_iter()
            .filter(|m| m.entities.iter().any(|e| wanted.contains(e.id.as_str())))
            .collect();
        Ok(out)
    }
}

/// 取 groups 中各 kept block 对应 interaction 的 memory_id (供 touch_access)。
fn sorted_groups_turn_ids(
    groups: &std::collections::BTreeMap<String, Vec<MemoryItem>>,
    kept: &[ContextBlock],
) -> Vec<String> {
    let mut out = Vec::new();
    for block in kept {
        if let Some(items) = groups.get(&block.interaction_id) {
            for it in items {
                out.push(it.id.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_core::{FusionMemoryEngine, MemoryTier, ToolCall};
    use fm_embed::StubEmbedder;

    fn tmp_engine(dim: usize) -> MemoryEngine {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("fm-engine-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Arc::new(StoreStub::open(&dir, dim).unwrap());
        let persist = Arc::new(Persist::open_in_memory().unwrap());
        let embedder: Arc<dyn Embedder> = Arc::new(StubEmbedder::new(dim));
        MemoryEngine::new(store, persist, embedder)
    }

    fn sample_interaction(ix_id: &str, turns: u32) -> Interaction {
        let mut t = Vec::new();
        for i in 0..turns {
            t.push(Turn {
                turn_idx: i,
                user_message: format!("user says {i}"),
                assistant_message: format!("assistant replies {i}"),
                tool_calls: vec![ToolCall {
                    name: "grep".into(),
                    args: serde_json::json!({}),
                    result_summary: "found".into(),
                }],
            });
        }
        Interaction {
            id: ix_id.into(),
            session_id: "sess-1".into(),
            turns: t,
            timestamp: 1000,
            metadata: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn commit_returns_one_id_per_turn() {
        let eng = tmp_engine(16);
        let ix = sample_interaction("ix-1", 3);
        let ids = eng.commit_episodic_memory("sess-1", &ix).await.unwrap();
        assert_eq!(ids.len(), 3);
        for id in &ids {
            let m = eng.get_memory(id.as_str()).await.unwrap().unwrap();
            assert_eq!(m.interaction_id, "ix-1");
        }
    }

    #[tokio::test]
    async fn retrieve_aggregates_by_interaction() {
        let eng = tmp_engine(16);
        let ix = sample_interaction("ix-2", 2);
        eng.commit_episodic_memory("sess-1", &ix).await.unwrap();
        let q = RetrieveQuery::new("user says 0", 10, 4096);
        let ctx = eng.retrieve_context(&q).await.unwrap();
        assert_eq!(ctx.blocks.len(), 1);
        assert_eq!(ctx.blocks[0].interaction_id, "ix-2");
        assert_eq!(ctx.blocks[0].turns.len(), 2);
    }

    #[tokio::test]
    async fn retrieve_ranks_relevant_higher() {
        let eng = tmp_engine(16);
        let a = sample_interaction("ix-a", 1);
        let b = Interaction {
            id: "ix-b".into(),
            session_id: "sess-1".into(),
            turns: vec![Turn {
                turn_idx: 0,
                user_message: "rust cargo build error".into(),
                assistant_message: "run cargo check".into(),
                tool_calls: vec![],
            }],
            timestamp: 2000,
            metadata: serde_json::json!({}),
        };
        eng.commit_episodic_memory("sess-1", &a).await.unwrap();
        eng.commit_episodic_memory("sess-1", &b).await.unwrap();
        let q = RetrieveQuery::new("rust cargo build error", 10, 4096);
        let ctx = eng.retrieve_context(&q).await.unwrap();
        assert_eq!(ctx.blocks[0].interaction_id, "ix-b");
    }

    #[tokio::test]
    async fn delete_tombstones() {
        let eng = tmp_engine(16);
        let ix = sample_interaction("ix-del", 1);
        let ids = eng.commit_episodic_memory("sess-1", &ix).await.unwrap();
        eng.delete_memory(ids[0].as_str()).await.unwrap();
        let q = RetrieveQuery::new("user says 0", 10, 4096);
        let ctx = eng.retrieve_context(&q).await.unwrap();
        assert!(ctx.blocks.is_empty());
    }

    #[tokio::test]
    async fn audit_finds_by_entity() {
        let eng = tmp_engine(16);
        let mut ix = sample_interaction("ix-aud", 1);
        ix.turns[0].user_message = "talk about Rust".into();
        let ids = eng.commit_episodic_memory("sess-1", &ix).await.unwrap();
        let mut m = eng.get_memory(ids[0].as_str()).await.unwrap().unwrap();
        m.entities.push(fm_core::EntityNode::new(
            "ent-rust".into(),
            "Rust".into(),
            fm_core::EntityType::Tech,
        ));
        eng.persist.put_memory(&m).unwrap();
        let found = eng.audit_memory_access(&["ent-rust".into()]).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].interaction_id, "ix-aud");
    }

    #[tokio::test]
    async fn tier_filter_applies() {
        let eng = tmp_engine(16);
        let ix = sample_interaction("ix-tier", 1);
        eng.commit_episodic_memory("sess-1", &ix).await.unwrap();
        let q = RetrieveQuery::new("user says 0", 10, 4096)
            .tier_filter_override(vec![MemoryTier::Long]);
        let ctx = eng.retrieve_context(&q).await.unwrap();
        assert!(ctx.blocks.is_empty());
    }

    // ---- 实体抽取注入测试 ----

    struct FakeExtractor {
        entities: Vec<fm_core::EntityNode>,
        success: bool,
    }
    #[async_trait::async_trait]
    impl EntityExtractor for FakeExtractor {
        async fn extract(&self, _turn_text: &str) -> ExtractResult {
            ExtractResult {
                entities: self.entities.clone(),
                success: self.success,
            }
        }
    }

    #[tokio::test]
    async fn commit_with_extractor_attaches_entities() {
        let mut eng = tmp_engine(16);
        let ext: Arc<dyn EntityExtractor> = Arc::new(FakeExtractor {
            entities: vec![fm_core::EntityNode::new(
                "ent-rust".into(),
                "Rust".into(),
                fm_core::EntityType::Tech,
            )],
            success: true,
        });
        eng = eng.with_extractor(ext);
        let mut ix = sample_interaction("ix-ext", 1);
        ix.turns[0].user_message = "I love Rust".into();
        let ids = eng.commit_episodic_memory("sess-1", &ix).await.unwrap();
        let m = eng.get_memory(ids[0].as_str()).await.unwrap().unwrap();
        assert_eq!(m.entities.len(), 1);
        assert_eq!(m.entities[0].name, "Rust");
        assert!(!m.entities_pending);
    }

    #[tokio::test]
    async fn commit_extractor_failure_keeps_pending() {
        // C5: 抽取失败 → entities_pending=true, content+vector 仍在
        let mut eng = tmp_engine(16);
        let ext: Arc<dyn EntityExtractor> = Arc::new(FakeExtractor {
            entities: vec![],
            success: false,
        });
        eng = eng.with_extractor(ext);
        let ix = sample_interaction("ix-fail", 1);
        let ids = eng.commit_episodic_memory("sess-1", &ix).await.unwrap();
        let m = eng.get_memory(ids[0].as_str()).await.unwrap().unwrap();
        assert!(m.entities_pending, "失败 → pending 保持");
        assert!(m.entities.is_empty());
        // content + vector 仍在
        assert!(!m.content.is_empty());
        assert!(!m.vector_ref.is_empty());
    }

    #[tokio::test]
    async fn consolidate_recycles_low_weight() {
        let eng = tmp_engine(16);
        let ix = sample_interaction("ix-rec", 1);
        let ids = eng.commit_episodic_memory("sess-1", &ix).await.unwrap();
        // 手动把 created_timestamp 设很久以前 → W(t)→0 < θ_drop
        let mut m = eng.get_memory(ids[0].as_str()).await.unwrap().unwrap();
        m.created_timestamp = 0;
        m.access_count = 0;
        eng.persist.put_memory(&m).unwrap();
        let report = eng.consolidate_memories().await.unwrap();
        assert!(report.dropped >= 1, "低 W(t) 应回收");
    }

    #[tokio::test]
    async fn consolidate_promotes_semantic_to_long() {
        let eng = tmp_engine(16);
        let ix = sample_interaction("ix-pro", 1);
        let ids = eng.commit_episodic_memory("sess-1", &ix).await.unwrap();
        // 改为 Semantic + Short → consolidate 应晋升 Long
        let mut m = eng.get_memory(ids[0].as_str()).await.unwrap().unwrap();
        m.memory_type = fm_core::MemoryType::Semantic;
        m.tier = MemoryTier::Short;
        m.weight = 0.8;
        eng.persist.put_memory(&m).unwrap();
        let report = eng.consolidate_memories().await.unwrap();
        assert!(report.promoted >= 1, "Semantic Short 应晋升 Long");
        let promoted = eng.get_memory(ids[0].as_str()).await.unwrap().unwrap();
        assert_eq!(promoted.tier, MemoryTier::Long);
    }

    #[tokio::test]
    async fn consolidate_reextracts_pending() {
        let mut eng = tmp_engine(16);
        let ext: Arc<dyn EntityExtractor> = Arc::new(FakeExtractor {
            entities: vec![fm_core::EntityNode::new(
                "ent-go".into(),
                "Go".into(),
                fm_core::EntityType::Tech,
            )],
            success: true,
        });
        eng = eng.with_extractor(ext);
        let mut ix = sample_interaction("ix-reex", 1);
        ix.turns[0].user_message = "use Go".into();
        let ids = eng.commit_episodic_memory("sess-1", &ix).await.unwrap();
        // commit 已抽出 → pending=false。手动重置 pending 模拟失败后重抽。
        let mut m = eng.get_memory(ids[0].as_str()).await.unwrap().unwrap();
        m.entities_pending = true;
        m.entities.clear();
        eng.persist.put_memory(&m).unwrap();
        let report = eng.consolidate_memories().await.unwrap();
        assert!(report.reextracted >= 1, "pending 应重抽");
        let after = eng.get_memory(ids[0].as_str()).await.unwrap().unwrap();
        assert!(!after.entities_pending);
        assert_eq!(after.entities[0].name, "Go");
    }

    trait RetrieveQueryExt {
        fn tier_filter_override(self, tiers: Vec<MemoryTier>) -> Self;
    }
    impl RetrieveQueryExt for RetrieveQuery {
        fn tier_filter_override(mut self, tiers: Vec<MemoryTier>) -> Self {
            self.tier_filter = Some(tiers);
            self
        }
    }
}
