//! 融合评分 + 衰减集成。PRD §6.4, B3。
//!
//! score = α*cosine + β*W(t) + γ*graph_affinity
//! α=0.5, β=0.3, γ=0.2。W(t) 由 fm_similarity::weight_at 算。

use fm_core::{EntityNode, MemoryItem, MemoryType};
use fm_graph::{GraphResult, GraphStore};
use fm_similarity::weight_at;
use tracing::debug;

pub const ALPHA: f64 = 0.5; // cosine
pub const BETA: f64 = 0.3; // 衰减 W(t)
pub const GAMMA: f64 = 0.2; // graph_affinity

// M1 #39: 四路融合权重 (evolution plan §2.1)。cosine 0.4 / bm25 0.3 / graph 0.2 / freshness 0.1。
// 三路 fuse_score (α/β/γ) 保留作消融对照臂, 生产检索走 fuse_score4。
pub const W4_COSINE: f64 = 0.4;
pub const W4_BM25: f64 = 0.3;
pub const W4_GRAPH: f64 = 0.2;
pub const W4_FRESHNESS: f64 = 0.1;

pub const GRAPH_HOP_LIMIT: usize = 2;

/// M3 #44: 过期记忆降权系数 (乘法叠加, 防线 3 — 只降不删)。
/// 有效期窗口与遗忘曲线相乘: score = fuse_score4 × validity。
/// 过期 ≠ tombstone — tombstone 仍只给显式删除; 过期项仍可被词法强匹配拉回。
pub const VALIDITY_EXPIRED_FACTOR: f64 = 0.1;

/// M3 #44: 事实有效期窗口因子 [0,1]。
/// - 无窗口 (valid_from/valid_until 均 None) → 1.0 (旧行为, 消融基线不受影响)
/// - 未生效 (now < valid_from) → 0.0 (未来事实不应浮出)
/// - 已过期 (now > valid_until) → VALIDITY_EXPIRED_FACTOR (降权不删除)
/// - 窗口内 → 1.0
pub fn validity_of(item: &MemoryItem, now: u64) -> f64 {
    if let Some(vf) = item.valid_from {
        if now < vf {
            return 0.0;
        }
    }
    if let Some(vu) = item.valid_until {
        if now > vu {
            return VALIDITY_EXPIRED_FACTOR;
        }
    }
    1.0
}

/// θ_drop: W(t) < 此值 → consolidate 回收 (tombstone)。
pub const THETA_DROP: f64 = 0.05;
/// θ_promote: Short→Long 晋升阈值 (Episodic W(t) 超此值可晋升)。
pub const THETA_PROMOTE: f64 = 0.3;
/// Episodic 7 天内召回 ≥ 此次数 → 晋升 Long。
pub const EPISODIC_RECALL_PROMOTE: u64 = 2;
pub const EPISODIC_RECALL_WINDOW_SECS: u64 = 7 * 86400;

/// v1.9.0 #53: 治理面板参数快照 — 检索/遗忘调优常量的只读视图 (Rule 5: 确定性,
/// 无模型调用)。`config` RPC 与 /app Governance 页共用。
pub fn governance_snapshot() -> serde_json::Value {
    serde_json::json!({
        "fusion": {
            "description": "four-way hybrid retrieval weights (fuse_score4)",
            "cosine": W4_COSINE,
            "bm25": W4_BM25,
            "graph": W4_GRAPH,
            "freshness": W4_FRESHNESS,
        },
        "three_way_ablation_arm": {
            "description": "legacy three-way fusion (ablation comparison arm)",
            "alpha_cosine": ALPHA,
            "beta_decay": BETA,
            "gamma_graph": GAMMA,
        },
        "validity": {
            "description": "M3 validity-window stacking (defense line 3)",
            "expired_factor": VALIDITY_EXPIRED_FACTOR,
        },
        "forgetting": {
            "description": "forgetting-curve consolidation thresholds",
            "theta_drop": THETA_DROP,
            "theta_promote": THETA_PROMOTE,
            "episodic_recall_promote": EPISODIC_RECALL_PROMOTE,
            "episodic_recall_window_secs": EPISODIC_RECALL_WINDOW_SECS,
        },
        "graph": {
            "hop_limit": GRAPH_HOP_LIMIT,
        },
    })
}

/// 纯函数融合评分 (无 DB)。query_entities/candidate_entities 的 id 用于 graph_affinity,
/// graph_affinity 由 caller 预算传入 (避免本函数依赖 Persist)。
pub fn fuse_score(cosine: f64, w_t: f64, graph_aff: f64) -> f64 {
    ALPHA * cosine + BETA * w_t + GAMMA * graph_aff
}

/// M1 #39: 四路融合评分。bm25 为调用方归一化后的 [0,1] 词法分 (normalize_bm25)。
pub fn fuse_score4(cosine: f64, bm25: f64, graph_aff: f64, freshness: f64) -> f64 {
    W4_COSINE * cosine + W4_BM25 * bm25 + W4_GRAPH * graph_aff + W4_FRESHNESS * freshness
}

/// M1 #39: 归一化 fts_search 的 BM25 rank 到 [0,1]。输入按 best-first 排序
/// (rank ASC, 越负越好)。best=1.0, worst=0.0, 线性内插; 单命中=1.0; 空输入返空 map。
pub fn normalize_bm25(ranks: &[(String, f64)]) -> std::collections::HashMap<String, f64> {
    let mut out = std::collections::HashMap::with_capacity(ranks.len());
    if ranks.is_empty() {
        return out;
    }
    let best = ranks[0].1;
    let worst = ranks[ranks.len() - 1].1;
    let span = worst - best;
    for (id, r) in ranks {
        let norm = if span.abs() < 1e-12 {
            1.0
        } else {
            (worst - r) / span
        };
        out.insert(id.clone(), norm);
    }
    out
}

/// 算某 MemoryItem 当前 W(t)。t = now - created_timestamp。
pub fn weight_of(item: &MemoryItem, now: u64) -> f64 {
    weight_at(
        item.weight,
        item.created_timestamp,
        now,
        item.memory_type,
        item.access_count,
    )
}

/// #25: 归一化新鲜度 — 融合评分专用 (retrieval quality ablation 裁定)。
/// exp(-t/τ(type)) * reinforce, w0=1 封顶 1.0。
/// 原始 W(t) 含 w0 (0.6/0.8/1.0 随 MemoryType), 加性注入融合分数会让
/// Procedural/Semantic 与相关性无关地系统性压过 Episodic — benchmark 实测
/// recall@5 0.48 → 0.17 (见 docs/benchmarks/retrieval-quality-2026-10-25.md)。
/// W(t) 原值仍用于 θ_drop/θ_promote 调度阈值 (PRD §7.3), 仅检索排序改用归一化值。
pub fn freshness_of(item: &MemoryItem, now: u64) -> f64 {
    let f = weight_at(
        1.0,
        item.created_timestamp,
        now,
        item.memory_type,
        item.access_count,
    );
    f.min(1.0)
}

/// 候选记忆的实体 id 列表。
pub fn entity_ids(entities: &[EntityNode]) -> Vec<String> {
    entities.iter().map(|e| e.id.clone()).collect()
}

/// 带 DB 的完整评分: 取 cosine + W(t) + graph_affinity。PRD §6.4。
/// §1.5: 取 `&dyn GraphStore` (非具体 Persist), 经 fm_graph::GraphStore trait 解耦图层。
pub fn score_candidate(
    store: &dyn GraphStore,
    cosine: f64,
    item: &MemoryItem,
    query_entity_ids: &[String],
    now: u64,
) -> GraphResult<f64> {
    // #25: 融合用归一化新鲜度 (剥 w0), 非 W(t) 原值 — 见 freshness_of 注释。
    let w_t = freshness_of(item, now);
    let cand_ids = entity_ids(&item.entities);
    let graph_aff = if query_entity_ids.is_empty() || cand_ids.is_empty() {
        0.0
    } else {
        fm_graph::graph_affinity(store, query_entity_ids, &cand_ids, GRAPH_HOP_LIMIT)?
    };
    let score = fuse_score(cosine, w_t, graph_aff);
    debug!(cosine, w_t, graph_aff, score, "candidate scored");
    Ok(score)
}

/// M1 #39: 四路评分 (检索主路径)。bm25 由 caller 从 fts_search+normalize_bm25 得出,
/// 候选未命中词路时传 0.0。graph_affinity 逻辑与三路版相同。
pub fn score_candidate4(
    store: &dyn GraphStore,
    cosine: f64,
    bm25: f64,
    item: &MemoryItem,
    query_entity_ids: &[String],
    now: u64,
) -> GraphResult<f64> {
    let freshness = freshness_of(item, now);
    let cand_ids = entity_ids(&item.entities);
    let graph_aff = if query_entity_ids.is_empty() || cand_ids.is_empty() {
        0.0
    } else {
        fm_graph::graph_affinity(store, query_entity_ids, &cand_ids, GRAPH_HOP_LIMIT)?
    };
    // M3 #44: 有效期窗口乘法叠加 (fuse_score4 已含加性 freshness 臂, validity 是独立乘性层)。
    let validity = validity_of(item, now);
    let score = fuse_score4(cosine, bm25, graph_aff, freshness) * validity;
    debug!(
        cosine,
        bm25, graph_aff, freshness, validity, score, "candidate scored (four-way)"
    );
    Ok(score)
}

/// 是否该回收 (遗忘)。W(t) < θ_drop。
pub fn should_recycle(item: &MemoryItem, now: u64) -> bool {
    weight_of(item, now) < THETA_DROP
}

/// 是否该晋升 Long。PRD §7.3。
/// Semantic/Procedural: 直接可晋升 (达到阈值)。
/// Episodic: 7 天内召回 ≥2 次 或 W(t) > θ_promote。
pub fn should_promote(item: &MemoryItem, now: u64) -> bool {
    match item.memory_type {
        MemoryType::Semantic | MemoryType::Procedural => true,
        MemoryType::Episodic => {
            let w = weight_of(item, now);
            if w > THETA_PROMOTE {
                return true;
            }
            let window_start = now.saturating_sub(EPISODIC_RECALL_WINDOW_SECS);
            item.last_accessed_timestamp >= window_start
                && item.access_count >= EPISODIC_RECALL_PROMOTE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_core::{EntityNode, EntityType, MemoryItem, MemoryTier, MemoryType};
    use fm_persist::Persist;

    fn mk_item(
        weight: f64,
        created: u64,
        access: u64,
        last_access: u64,
        mt: MemoryType,
    ) -> MemoryItem {
        let mut m = MemoryItem::new_turn_skeleton(
            "m1".into(),
            "ix".into(),
            0,
            "s".into(),
            String::new(),
            mt,
            "c".into(),
            created,
        );
        m.tier = MemoryTier::Short;
        m.weight = weight;
        m.access_count = access;
        m.last_accessed_timestamp = last_access;
        m
    }

    #[test]
    fn fuse_score_weights() {
        let s = fuse_score(1.0, 1.0, 1.0);
        assert!((s - 1.0).abs() < 1e-9, "全 1.0 → 1.0");
        let s2 = fuse_score(0.8, 0.0, 0.0);
        assert!((s2 - 0.4).abs() < 1e-9, "纯 cosine 0.8 → 0.4");
    }

    #[test]
    fn fuse_score4_weights() {
        // #39: 全 1.0 → 权重和 1.0
        let s = fuse_score4(1.0, 1.0, 1.0, 1.0);
        assert!((s - 1.0).abs() < 1e-9, "四路全 1.0 → 1.0");
        // 纯词法命中 (FTS-only candidate, cosine=0): 0.3*1 + 0.1*fresh
        let s2 = fuse_score4(0.0, 1.0, 0.0, 0.5);
        assert!((s2 - 0.35).abs() < 1e-9, "bm25=1 fresh=0.5 → 0.35");
    }

    #[test]
    fn normalize_bm25_best_first() {
        let ranks = vec![
            ("a".to_string(), -7.5),
            ("b".to_string(), -4.5),
            ("c".to_string(), -1.5),
        ];
        let m = normalize_bm25(&ranks);
        assert!((m["a"] - 1.0).abs() < 1e-9, "best → 1.0");
        assert!((m["c"] - 0.0).abs() < 1e-9, "worst → 0.0");
        assert!((m["b"] - 0.5).abs() < 1e-9, "mid → 0.5");
    }

    #[test]
    fn normalize_bm25_single_and_empty() {
        let m1 = normalize_bm25(&[("x".to_string(), -3.0)]);
        assert!((m1["x"] - 1.0).abs() < 1e-9, "单命中 → 1.0");
        assert!(normalize_bm25(&[]).is_empty(), "空输入 → 空 map");
    }

    #[test]
    fn validity_of_window_semantics() {
        // M3 #44: 无窗口 → 1.0
        let item = mk_item(1.0, 0, 0, 0, MemoryType::Episodic);
        assert!(
            (validity_of(&item, 5_000) - 1.0).abs() < 1e-9,
            "no window → 1.0"
        );
        // 未生效: now < valid_from → 0.0
        let fut = item.clone().with_validity(Some(9_000), None);
        assert!(
            (validity_of(&fut, 5_000) - 0.0).abs() < 1e-9,
            "not yet valid → 0.0"
        );
        assert!(
            (validity_of(&fut, 9_000) - 1.0).abs() < 1e-9,
            "at valid_from → 1.0"
        );
        // 已过期: now > valid_until → 降权 0.1 (不删除)
        let exp = item.clone().with_validity(None, Some(1_000));
        assert!(
            (validity_of(&exp, 5_000) - VALIDITY_EXPIRED_FACTOR).abs() < 1e-9,
            "expired → 0.1"
        );
        // 窗口内 → 1.0
        let inwin = item.with_validity(Some(1_000), Some(9_000));
        assert!(
            (validity_of(&inwin, 5_000) - 1.0).abs() < 1e-9,
            "in window → 1.0"
        );
    }

    #[test]
    fn score_candidate4_multiplies_validity() {
        // 消融: 同候选, 有过期窗口 → 分数 = 无窗口 × 0.1 (乘法叠加, 防线 3)
        let dir = tempfile::tempdir().unwrap();
        let persist = Persist::open(dir.path().join("m.db")).unwrap();
        let base = mk_item(1.0, 0, 0, 0, MemoryType::Episodic);
        let expired = base.clone().with_validity(None, Some(1));
        let s_open = score_candidate4(&persist, 0.8, 0.0, &base, &[], 100_000).unwrap();
        let s_exp = score_candidate4(&persist, 0.8, 0.0, &expired, &[], 100_000).unwrap();
        assert!(
            (s_exp - s_open * VALIDITY_EXPIRED_FACTOR).abs() < 1e-9,
            "expired score = open × factor"
        );
    }

    #[test]
    fn weight_of_uses_created_not_accessed() {
        // t = now - created (B3 解耦)
        let item = mk_item(1.0, 1000, 0, 9999, MemoryType::Episodic);
        let w = weight_of(&item, 1000); // t=0 → 1.0 * 1.0 * reinforce(1.0)
        assert!((w - 1.0).abs() < 1e-6, "t=0 → W0*1*reinforce");
    }

    #[test]
    fn should_recycle_low_weight() {
        // Episodic tau=86400, t 很大 → W(t)→0 < θ_drop
        let item = mk_item(0.6, 0, 0, 0, MemoryType::Episodic);
        assert!(should_recycle(&item, 10 * 86400));
    }

    #[test]
    fn should_not_recycle_fresh() {
        let item = mk_item(0.6, 1000, 0, 1000, MemoryType::Episodic);
        assert!(!should_recycle(&item, 1000));
    }

    #[test]
    fn promote_semantic_direct() {
        let item = mk_item(0.8, 1000, 0, 1000, MemoryType::Semantic);
        assert!(should_promote(&item, 1000));
    }

    #[test]
    fn promote_procedural_direct() {
        let item = mk_item(1.0, 1000, 0, 1000, MemoryType::Procedural);
        assert!(should_promote(&item, 1000));
    }

    #[test]
    fn promote_episodic_high_weight() {
        // Episodic W0=0.6, t 小 → W(t) > θ_promote=0.3
        let item = mk_item(0.6, 1000, 0, 1000, MemoryType::Episodic);
        assert!(should_promote(&item, 1000));
    }

    #[test]
    fn promote_episodic_recall_count() {
        // W(t) 低但 7 天内召回 2 次
        let now = 1_000_000;
        let created = 0; // 很久, W(t) 低
        let item = mk_item(0.6, created, 2, now - 100, MemoryType::Episodic);
        // 确认 W(t) 确实低 (不至于走 high_weight 分支)
        assert!(
            weight_of(&item, now) < THETA_PROMOTE,
            "需低 W(t) 才测 recall 分支"
        );
        assert!(should_promote(&item, now), "7天内召回≥2 应晋升");
    }

    #[test]
    fn no_promote_episodic_stale_low_recall() {
        let now = 1_000_000;
        let item = mk_item(0.6, 0, 1, 100, MemoryType::Episodic); // recall 1, 很久前
        assert!(!should_promote(&item, now));
    }

    #[test]
    fn entity_ids_extracted() {
        let e = vec![
            EntityNode::new("a".into(), "A".into(), EntityType::Tech),
            EntityNode::new("b".into(), "B".into(), EntityType::Tech),
        ];
        assert_eq!(entity_ids(&e), vec!["a", "b"]);
    }

    #[test]
    fn score_candidate_zero_graph_when_no_entities() {
        let p = Persist::open_in_memory().unwrap();
        let item = mk_item(0.8, 1000, 0, 1000, MemoryType::Semantic);
        let s = score_candidate(&p, 0.9, &item, &[], 1000).unwrap();
        // graph_aff=0 → 0.5*0.9 + 0.3*W(t) + 0
        assert!(s > 0.0);
    }
}
