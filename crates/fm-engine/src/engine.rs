//! MemoryEngine: FusionMemoryEngine 实现。PRD §6, §11.1, §11.4, B3/C5。
//!
//! commit: turn 级拆分 → embed → 同步写 persist(WAL)+store(向量) → 异步抽实体补 entities。
//! retrieve: query→embed→store KNN→persist 聚合 interaction_id → 融合评分排序。
//! consolidate: W(t)<θ_drop 回收 tombstone; Short→Long 晋升; entities_pending 批量重抽。
//! Embedder/EntityExtractor 注入 (测试用 stub, 生产用 mlx)。

use std::sync::Arc;

use async_trait::async_trait;
use fm_core::{
    ConsolidationFailure, ConsolidationReport, ContextBlock, FormattedContext, Interaction,
    MemoryId, MemoryItem, MemoryTier, MemoryType, RetrieveQuery, Turn,
};
use fm_embed::{vector_id_from_ulid, Embedder};
use fm_persist::{MergeLogEntry, Persist};
use fm_store::{FusionStoreEngine, StoreStub};
use tracing::{debug, info, warn};

use crate::entity_extract::{chat_completion, EntityExtractor, ExtractConfig, ExtractResult};
use crate::scoring;

const AGG_MAX_TURNS: usize = 20;
const MERGE_SIM_THRESHOLD: f64 = 0.92;
const MERGE_KNN: usize = 5;
const SUMMARIZE_MIN_EPISODIC: usize = 3;

pub struct MemoryEngine {
    store: Arc<StoreStub>,
    persist: Arc<Persist>,
    embedder: Arc<dyn Embedder>,
    extractor: Option<Arc<dyn EntityExtractor>>,
    extract_config: Option<ExtractConfig>,
}

impl MemoryEngine {
    pub fn new(store: Arc<StoreStub>, persist: Arc<Persist>, embedder: Arc<dyn Embedder>) -> Self {
        Self {
            store,
            persist,
            embedder,
            extractor: None,
            extract_config: None,
        }
    }

    /// 注入实体抽取器 (生产用 MlxEntityExtractor)。不注入则 entities 永远 pending。
    pub fn with_extractor(mut self, extractor: Arc<dyn EntityExtractor>) -> Self {
        self.extractor = Some(extractor);
        self
    }

    /// 注入抽取器 + chat 配置 (summarize 复用)。生产路径推荐。
    pub fn with_extractor_and_config(
        mut self,
        extractor: Arc<dyn EntityExtractor>,
        config: ExtractConfig,
    ) -> Self {
        self.extractor = Some(extractor);
        self.extract_config = Some(config);
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

    // ---- M3 consolidate saga 子步骤 ----

    /// 图合并: Semantic 记忆 ANN top-5 召回, sim>0.92 且共享 ≥1 实体 → 合并入 target, source tombstone。
    async fn consolidate_merge(
        &self,
        changed: &[MemoryItem],
        at: u64,
        failures: &mut Vec<ConsolidationFailure>,
    ) -> fm_core::MemoryResult<usize> {
        let mut merged = 0usize;
        let semantic: Vec<&MemoryItem> = changed
            .iter()
            .filter(|m| m.memory_type == MemoryType::Semantic && !m.tombstone)
            .collect();
        for src in semantic {
            if src.tombstone {
                continue;
            }
            let Ok(vr) = src.vector_ref.parse::<u64>() else {
                continue;
            };
            let Ok(Some(vec)) = self.store.get_vector(vr) else {
                continue;
            };
            let knn = self.store.search_knn(&vec, MERGE_KNN)?;
            for (vid, sim) in &knn {
                if (*sim as f64) < MERGE_SIM_THRESHOLD || (*vid) == vr {
                    continue;
                }
                // 找对应 memory (向量 ref → memory)
                let target = self
                    .persist
                    .list_all()
                    .map_err(|e| e.to_memory())?
                    .into_iter()
                    .find(|m| m.vector_ref == vid.to_string() && !m.tombstone);
                let Some(tgt) = target else { continue };
                if tgt.id == src.id {
                    continue;
                }
                // 共享 ≥1 实体 (A5: 同 type 精确 id 命中)
                let shared = src
                    .entities
                    .iter()
                    .any(|e| tgt.entities.iter().any(|t| t.id == e.id));
                if !shared {
                    continue;
                }
                // 合并: source tombstone + 记 merge_log
                if let Err(e) = self.persist.tombstone_memory(&src.id) {
                    failures.push(ConsolidationFailure {
                        memory_id: src.id.clone(),
                        stage: "merge".into(),
                        error: e.to_string(),
                    });
                    continue;
                }
                let _ = self.store.delete_vector(vr);
                if let Err(e) =
                    self.persist
                        .record_merge(&src.id, &tgt.id, "ann-sim+shared-entity", at)
                {
                    failures.push(ConsolidationFailure {
                        memory_id: src.id.clone(),
                        stage: "merge-log".into(),
                        error: e.to_string(),
                    });
                }
                merged += 1;
                info!(source = %src.id, target = %tgt.id, sim, "merged");
                break; // 每个 source 只合并一次
            }
        }
        Ok(merged)
    }

    /// 摘要压缩: 同 session ≥3 条 Episodic → fusion-mlx 生成摘要 → 新 Semantic 记忆。
    async fn consolidate_summarize(
        &self,
        changed: &[MemoryItem],
        at: u64,
        failures: &mut Vec<ConsolidationFailure>,
    ) -> fm_core::MemoryResult<usize> {
        let Some(cfg) = &self.extract_config else {
            debug!("no extract_config, summarize skipped");
            return Ok(0);
        };
        // 按 session 聚合本次变更的 Episodic
        let mut by_session: std::collections::HashMap<String, Vec<&MemoryItem>> =
            std::collections::HashMap::new();
        for m in changed {
            if m.memory_type == MemoryType::Episodic && !m.tombstone {
                by_session.entry(m.session_id.clone()).or_default().push(m);
            }
        }
        let mut summarized = 0usize;
        for (sess, items) in by_session {
            if items.len() < SUMMARIZE_MIN_EPISODIC {
                continue;
            }
            let joined = items
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n---\n");
            let summary = match chat_completion(
                cfg,
                "你是记忆摘要器, 把多轮对话压缩成一条简洁事实陈述, 不加额外信息。",
                &joined,
            )
            .await
            {
                Some(s) if !s.trim().is_empty() => s,
                _ => {
                    warn!(session = %sess, "summarize mlx returned empty, skip");
                    continue;
                }
            };
            // 写新 Semantic 记忆
            let id = Self::new_ulid();
            let vec = self
                .embedder
                .embed(&summary)
                .await
                .map_err(|e| e.to_memory())?;
            let vec_id = vector_id_from_ulid(&id);
            self.store.insert_vector(vec_id, &vec)?;
            let mut item = MemoryItem::new_turn_skeleton(
                id.clone(),
                format!("summary-{sess}"),
                0,
                sess.clone(),
                MemoryType::Semantic,
                summary,
                at,
            );
            item.vector_ref = vec_id.to_string();
            item.tier = MemoryTier::Long;
            item.entities_pending = true;
            if let Err(e) = self.persist.put_memory(&item) {
                failures.push(ConsolidationFailure {
                    memory_id: id,
                    stage: "summarize".into(),
                    error: e.to_string(),
                });
                continue;
            }
            self.persist
                .append_wop("summarize", &id, at)
                .map_err(|e| e.to_memory())?;
            summarized += 1;
            info!(session = %sess, "summarized into new semantic");
        }
        Ok(summarized)
    }

    /// 跨库对账: SQLite id ↔ store 向量 id 差异 → reconcile_report; tombstone 一致 → 物理删。
    fn consolidate_reconcile(
        &self,
        at: u64,
        failures: &mut Vec<ConsolidationFailure>,
    ) -> fm_core::MemoryResult<usize> {
        let sqlite_all = self.persist.list_all().map_err(|e| e.to_memory())?;
        // store-stub 无枚举 API → 用 SQLite 驱动 get_vector 逐条校验存在性。
        let mut reconciled = 0usize;
        for m in &sqlite_all {
            let Ok(vr) = m.vector_ref.parse::<u64>() else {
                continue;
            };
            if self.store.get_vector(vr).ok().flatten().is_none() {
                // SQLite 有向量 ref 但 store 无 → 悬空, 落 report
                let _ = self.persist.append_reconcile(
                    at,
                    &m.id,
                    "dangling-vector",
                    "sqlite has ref, store missing",
                );
            }
        }
        // tombstone 三库一致 → 物理删 (但跳过 merge_log 中 source, 留待 unmerge 可恢复)
        let merge_sources: std::collections::HashSet<String> = self
            .persist
            .list_merge_log()
            .map_err(|e| e.to_memory())?
            .into_iter()
            .map(|m| m.source_id)
            .collect();
        let tombs = self.persist.list_tombstoned().map_err(|e| e.to_memory())?;
        for t in &tombs {
            if merge_sources.contains(&t.id) {
                continue; // 合并产生 tombstone, 留给 unmerge 可回滚
            }
            let vr_ok = t
                .vector_ref
                .parse::<u64>()
                .map(|vr| self.store.delete_vector(vr).is_ok())
                .unwrap_or(true);
            if vr_ok {
                if let Err(e) = self.persist.physical_delete(&t.id) {
                    failures.push(ConsolidationFailure {
                        memory_id: t.id.clone(),
                        stage: "reconcile-physical-delete".into(),
                        error: e.to_string(),
                    });
                } else {
                    reconciled += 1;
                }
            }
        }
        Ok(reconciled)
    }

    /// fm-cli unmerge: 回滚合并, 恢复 source 记忆 (untombstone + 重建向量)。
    pub async fn unmerge(&self, merge_id: u64) -> fm_core::MemoryResult<bool> {
        let pair = self.persist.unmerge(merge_id).map_err(|e| e.to_memory())?;
        let Some((source_id, _target_id)) = pair else {
            return Ok(false);
        };
        // 恢复 source: untombstone (重写 tombstone=false)。向量需重建 (合并时已删)。
        if let Some(mut item) = self
            .persist
            .get_memory(&source_id)
            .map_err(|e| e.to_memory())?
        {
            item.tombstone = false;
            // 重建向量 (merge 时 delete_vector 了)
            if let Ok(vr) = item.vector_ref.parse::<u64>() {
                if self.store.get_vector(vr).unwrap_or(None).is_none() {
                    if let Ok(v) = self.embedder.embed(&item.content).await {
                        let _ = self.store.insert_vector(vr, &v);
                    }
                }
            }
            self.persist.put_memory(&item).map_err(|e| e.to_memory())?;
            info!(merge_id, source = %source_id, "unmerge restored source");
            return Ok(true);
        }
        Ok(false)
    }

    /// fm-cli reconcile: 手动触发跨库对账, 返回 reconcile_report 差异数。
    pub fn reconcile(&self) -> fm_core::MemoryResult<usize> {
        let at = Self::now_ms();
        let mut failures = Vec::new();
        self.consolidate_reconcile(at, &mut failures)
    }

    /// fm-cli: 列全部 merge_log。
    pub fn list_merges(&self) -> fm_core::MemoryResult<Vec<MergeLogEntry>> {
        self.persist.list_merge_log().map_err(|e| e.to_memory())
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
        let since = self
            .persist
            .last_consolidate_at()
            .map_err(|e| e.to_memory())?;
        let mut failures: Vec<ConsolidationFailure> = Vec::new();
        let mut dropped = 0usize;
        let mut promoted = 0usize;
        let mut reextracted = 0usize;

        // 增量变更集 (B4: 仅处理 last_consolidate_at 以来变更)。
        let changed = self
            .persist
            .list_changed_since(since)
            .map_err(|e| e.to_memory())?;

        // 1. 衰减回收 + 晋升 (增量)
        for m in &changed {
            if scoring::should_recycle(m, now) {
                if let Ok(vr) = m.vector_ref.parse::<u64>() {
                    if let Err(e) = self.store.delete_vector(vr) {
                        warn!(id = %m.id, error = %e, "recycle delete_vector failed");
                        failures.push(ConsolidationFailure {
                            memory_id: m.id.clone(),
                            stage: "recycle".into(),
                            error: e.to_string(),
                        });
                    }
                }
                if let Err(e) = self.persist.tombstone_memory(&m.id) {
                    failures.push(ConsolidationFailure {
                        memory_id: m.id.clone(),
                        stage: "recycle".into(),
                        error: e.to_string(),
                    });
                }
                dropped += 1;
                continue;
            }
            if m.tier == MemoryTier::Short && scoring::should_promote(m, now) {
                let mut up = m.clone();
                up.tier = MemoryTier::Long;
                if let Err(e) = self.persist.put_memory(&up) {
                    failures.push(ConsolidationFailure {
                        memory_id: m.id.clone(),
                        stage: "promote".into(),
                        error: e.to_string(),
                    });
                } else {
                    promoted += 1;
                }
            }
        }

        // 2. 图合并 (ANN top-5, sim>0.92 + 共享实体) — 仅 Semantic
        let merged = self
            .consolidate_merge(&changed, start, &mut failures)
            .await?;

        // 3. 摘要压缩 (同 session 高密度 Episodic 序列 → fusion-mlx → 新 Semantic)
        let summarized = self
            .consolidate_summarize(&changed, start, &mut failures)
            .await?;

        // 4. 延迟补抽 (C5)
        for m in &changed {
            if m.entities_pending {
                if let Some(extractor) = &self.extractor {
                    let ExtractResult { entities, success } = extractor.extract(&m.content).await;
                    if success {
                        let mut item = m.clone();
                        item.entities = entities;
                        item.entities_pending = false;
                        if let Err(e) = self.persist.put_memory(&item) {
                            failures.push(ConsolidationFailure {
                                memory_id: m.id.clone(),
                                stage: "reextract".into(),
                                error: e.to_string(),
                            });
                        } else {
                            reextracted += 1;
                        }
                    }
                }
            }
        }

        // 5. 跨库对账 (A1): SQLite memory_item.id ↔ store 向量 id 差异 → reconcile_report;
        //    tombstone 且三库一致 → 物理删。
        let reconciled = self.consolidate_reconcile(start, &mut failures)?;

        let report = ConsolidationReport {
            elapsed_ms: Self::now_ms().saturating_sub(start),
            dropped,
            promoted,
            merged,
            summarized,
            reextracted,
            reconciled,
            failures,
        };
        self.persist
            .record_consolidation(&report, start)
            .map_err(|e| e.to_memory())?;
        info!(
            dropped,
            promoted, merged, summarized, reextracted, reconciled, "consolidate done"
        );
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

    // ---- M3 saga 测试 ----

    async fn semantic_item(
        eng: &MemoryEngine,
        id: &str,
        content: &str,
        entity_id: &str,
    ) -> MemoryItem {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(1_000_000_000);
        let mut m = MemoryItem::new_turn_skeleton(
            id.into(),
            format!("ix-{id}"),
            0,
            "sess-1".into(),
            MemoryType::Semantic,
            content.into(),
            now,
        );
        m.tier = MemoryTier::Long;
        m.entities.push(fm_core::EntityNode::new(
            entity_id.into(),
            entity_id.into(),
            fm_core::EntityType::Tech,
        ));
        // 直接写 persist + store (不走 commit 的 embedding, 用 stub 确定向量)
        let vec = eng.embedder.embed(content).await.unwrap();
        let vid = fm_embed::vector_id_from_ulid(id);
        eng.store.insert_vector(vid, &vec).unwrap();
        m.vector_ref = vid.to_string();
        eng.persist.put_memory(&m).unwrap();
        m
    }

    #[tokio::test]
    async fn consolidate_merges_similar_semantic() {
        let eng = tmp_engine(16);
        // 同实体 + 同内容 (stub 向量 sim=1.0) → 合并
        semantic_item(&eng, "m-a", "rust cargo build error", "ent-rust").await;
        semantic_item(&eng, "m-b", "rust cargo build error", "ent-rust").await;
        let report = eng.consolidate_memories().await.unwrap();
        assert!(
            report.merged >= 1,
            "相似同实体应合并 merged={}",
            report.merged
        );
        let merges = eng.list_merges().unwrap();
        assert!(!merges.is_empty());
    }

    #[tokio::test]
    async fn consolidate_no_merge_different_entity() {
        let eng = tmp_engine(16);
        semantic_item(&eng, "m-c", "rust cargo build error", "ent-rust").await;
        semantic_item(&eng, "m-d", "rust cargo build error", "ent-go").await;
        let report = eng.consolidate_memories().await.unwrap();
        assert_eq!(report.merged, 0, "不同实体不应合并");
    }

    #[tokio::test]
    async fn unmerge_restores_source() {
        let eng = tmp_engine(16);
        semantic_item(&eng, "m-u1", "rust cargo build error", "ent-rust").await;
        semantic_item(&eng, "m-u2", "rust cargo build error", "ent-rust").await;
        eng.consolidate_memories().await.unwrap();
        let merges = eng.list_merges().unwrap();
        assert!(!merges.is_empty(), "应先有合并");
        let mid = merges[0].id;
        // source 已 tombstone
        let src_before = eng.get_memory(&merges[0].source_id).await.unwrap().unwrap();
        assert!(src_before.tombstone);
        let ok = eng.unmerge(mid).await.unwrap();
        assert!(ok, "unmerge 应成功");
        let src_after = eng.get_memory(&merges[0].source_id).await.unwrap().unwrap();
        assert!(!src_after.tombstone, "unmerge 后 source 应恢复");
        // merge_log 行应已删
        assert!(eng.list_merges().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unmerge_unknown_id_returns_false() {
        let eng = tmp_engine(16);
        let ok = eng.unmerge(99999).await.unwrap();
        assert!(!ok);
    }

    #[tokio::test]
    async fn reconcile_physical_deletes_tombstoned() {
        let eng = tmp_engine(16);
        let m = semantic_item(&eng, "m-rd", "unique content here", "ent-x").await;
        eng.persist.tombstone_memory(&m.id).unwrap();
        // tombstone 在 list_all 排除, 但 list_tombstoned 含
        let n = eng.reconcile().unwrap();
        // reconcile 触发物理删
        // 再次 get 应 None
        let gone = eng.persist.get_memory(&m.id).unwrap();
        assert!(gone.is_none(), "tombstone 物理删后应 None");
        assert!(n >= 1);
    }

    #[tokio::test]
    async fn consolidate_incremental_skips_unchanged() {
        let eng = tmp_engine(16);
        // 第一轮 consolidate 建基线
        semantic_item(&eng, "m-i1", "baseline content", "ent-b").await;
        let _r1 = eng.consolidate_memories().await.unwrap();
        // 无新增变更 → 第二轮 dropped/promoted/merged 应 0
        let r2 = eng.consolidate_memories().await.unwrap();
        assert_eq!(r2.dropped, 0);
        assert_eq!(r2.merged, 0);
        assert_eq!(r2.summarized, 0);
    }

    #[tokio::test]
    async fn persist_merge_log_roundtrip() {
        let p = fm_persist::Persist::open_in_memory().unwrap();
        let now = 12345u64;
        p.record_merge("src-1", "tgt-1", "test", now).unwrap();
        let log = p.list_merge_log().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].source_id, "src-1");
        assert_eq!(log[0].target_id, "tgt-1");
        let pair = p.unmerge(log[0].id).unwrap();
        assert_eq!(pair, Some(("src-1".into(), "tgt-1".into())));
        assert!(p.list_merge_log().unwrap().is_empty());
    }

    #[tokio::test]
    async fn persist_changed_since_and_tombstoned() {
        let p = fm_persist::Persist::open_in_memory().unwrap();
        let mut a = MemoryItem::new_turn_skeleton(
            "a".into(),
            "ix".into(),
            0,
            "s".into(),
            MemoryType::Episodic,
            "ca".into(),
            100,
        );
        a.last_accessed_timestamp = 200;
        p.put_memory(&a).unwrap();
        let changed = p.list_changed_since(150).unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].id, "a");
        p.tombstone_memory("a").unwrap();
        let tombs = p.list_tombstoned().unwrap();
        assert_eq!(tombs.len(), 1);
        assert_eq!(tombs[0].id, "a");
    }

    #[tokio::test]
    async fn reconcile_reports_dangling_vector_ref() {
        // SQLite 有 vector_ref 但 store 无对应向量 → 落 reconcile_report (dangling-vector)
        let eng = tmp_engine(16);
        let m = semantic_item(&eng, "m-dangle", "dangling content", "ent-d").await;
        // 删 store 向量, 制造悬空
        let vid: u64 = m.vector_ref.parse().unwrap();
        eng.store.delete_vector(vid).unwrap();
        // get_memory 走 persist; list_all 含; reconcile 走 get_vector 校验
        let n = eng.reconcile().unwrap();
        // dangling 不计 reconciled (未物理删, 仅落 report); tombstone 物理删计数另算
        // 这里 m-dangle 非 tombstone → 不会被物理删, n 应 0
        assert_eq!(n, 0);
        // 记忆仍在
        assert!(eng.persist.get_memory(&m.id).unwrap().is_some());
    }

    #[tokio::test]
    async fn reconcile_deletes_tombstone_with_bad_vector_ref() {
        // tombstone 的 vector_ref 非数字 → unwrap_or(true) 仍物理删
        let eng = tmp_engine(16);
        let mut m = semantic_item(&eng, "m-badref", "badref content", "ent-br").await;
        // 篡改 vector_ref 为非数字 + 置 tombstone (put_memory 会覆盖, 故手动设)
        m.vector_ref = "not-a-number".into();
        m.tombstone = true;
        eng.persist.put_memory(&m).unwrap();
        let n = eng.reconcile().unwrap();
        assert!(n >= 1, "非数字 vector_ref 的 tombstone 应物理删");
        assert!(eng.persist.get_memory(&m.id).unwrap().is_none());
    }

    #[tokio::test]
    async fn consolidate_summarize_skips_without_config() {
        // 无 extract_config (tmp_engine 无 config) → summarize 直接返 0, 不分组不调 mlx
        let eng = tmp_engine(16);
        // 同 session 提交 4 条 episodic (≥SUMMARIZE_MIN_EPISODIC=3), 无 config → summarized=0
        let ix = sample_interaction("ix-sum", 4);
        eng.commit_episodic_memory("sess-1", &ix).await.unwrap();
        let report = eng.consolidate_memories().await.unwrap();
        assert_eq!(report.summarized, 0, "无 config 不应 summarize");
    }

    #[tokio::test]
    async fn unmerge_missing_source_row_returns_false() {
        // merge_log 行存在但 source 记忆已物理删 (get_memory None) → unmerge 返 false
        let eng = tmp_engine(16);
        // 手写一条 merge_log, source 指向不存在的 id
        eng.persist
            .record_merge("ghost-src", "ghost-tgt", "test", 1)
            .unwrap();
        let mid = eng.list_merges().unwrap()[0].id;
        let ok = eng.unmerge(mid).await.unwrap();
        assert!(!ok, "source 行不存在 → unmerge 返 false");
    }
}
