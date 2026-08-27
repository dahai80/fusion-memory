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

pub const GRAPH_HOP_LIMIT: usize = 2;

/// θ_drop: W(t) < 此值 → consolidate 回收 (tombstone)。
pub const THETA_DROP: f64 = 0.05;
/// θ_promote: Short→Long 晋升阈值 (Episodic W(t) 超此值可晋升)。
pub const THETA_PROMOTE: f64 = 0.3;
/// Episodic 7 天内召回 ≥ 此次数 → 晋升 Long。
pub const EPISODIC_RECALL_PROMOTE: u64 = 2;
pub const EPISODIC_RECALL_WINDOW_SECS: u64 = 7 * 86400;

/// 纯函数融合评分 (无 DB)。query_entities/candidate_entities 的 id 用于 graph_affinity,
/// graph_affinity 由 caller 预算传入 (避免本函数依赖 Persist)。
pub fn fuse_score(cosine: f64, w_t: f64, graph_aff: f64) -> f64 {
    ALPHA * cosine + BETA * w_t + GAMMA * graph_aff
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
    let w_t = weight_of(item, now);
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
