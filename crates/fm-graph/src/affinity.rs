//! graph_affinity: N-hop 图亲和度。PRD §7.2, B4 (GRAPH_HOP_LIMIT=2)。
//!
//! 命中即停, 取最大亲和:
//! - 候选实体 ∈ query 实体集 → 1.0 (直命中)
//! - 候选实体在 query 实体的 N-hop 可达集内, 最短跳 h → 0.5^h (hop1=0.5, hop2=0.25)
//! - 否则 → 0.0
//!
//! DB 遍历下沉 fm-persist::Persist::n_hop_reachable (WITH RECURSIVE CTE)。
//!
//! §1.5: graph_affinity 取 `&dyn GraphStore` (非具体 Persist), 图层可注入内存 mock 单测。
//! Persist 适配在 fm_graph::store (impl GraphStore for Persist)。

use tracing::debug;

use crate::error::GraphResult;
use crate::store::GraphStore;

pub const GRAPH_HOP_LIMIT: usize = 2;

/// 纯函数: 由可达集算亲和度。无 DB, 便于单测。
/// reachable = 从某 query 实体出发 hop_limit 跳内可达 (node, hop)。
pub fn affinity_from_reachability(
    query_ids: &[String],
    candidate_ids: &[String],
    reachable: &[(String, usize)],
) -> f64 {
    if query_ids.is_empty() || candidate_ids.is_empty() {
        return 0.0;
    }
    let qset: Vec<&str> = query_ids.iter().map(|s| s.as_str()).collect();
    let cset: Vec<&str> = candidate_ids.iter().map(|s| s.as_str()).collect();

    // 直命中优先
    for q in &qset {
        for c in &cset {
            if *q == *c {
                return 1.0;
            }
        }
    }

    // 图可达: 候选出现在可达集, 取最短跳
    let mut best: f64 = 0.0;
    for (node, hop) in reachable {
        if cset.contains(&node.as_str()) {
            let score = 0.5_f64.powi(*hop as i32);
            if score > best {
                best = score;
            }
        }
    }
    best
}

/// DB 版: 对每个 query 实体查 N-hop 可达, 取最大亲和。PRD §7.2。
/// §1.5: 取 `&dyn GraphStore` (非具体 Persist), 图层可注入内存 mock 单测。
pub fn graph_affinity(
    store: &dyn GraphStore,
    query_ids: &[String],
    candidate_ids: &[String],
    hop_limit: usize,
) -> GraphResult<f64> {
    if query_ids.is_empty() || candidate_ids.is_empty() {
        return Ok(0.0);
    }
    let hop_limit = hop_limit.clamp(1, GRAPH_HOP_LIMIT);

    let mut best: f64 = 0.0;
    for q in query_ids {
        // 直命中
        if candidate_ids.iter().any(|c| c == q) {
            return Ok(1.0);
        }
        let reachable = store.n_hop_reachable(q, hop_limit)?;
        let aff = affinity_from_reachability(std::slice::from_ref(q), candidate_ids, &reachable);
        if aff > best {
            best = aff;
        }
    }
    debug!(best, "graph_affinity computed");
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_core::{EntityNode, EntityType, MemoryItem, MemoryTier, MemoryType};
    use fm_persist::Persist;

    fn mk_item(id: &str, entities: Vec<EntityNode>) -> MemoryItem {
        let mut m = MemoryItem::new_turn_skeleton(
            id.into(),
            "ix".into(),
            0,
            "s".into(),
            String::new(),
            MemoryType::Semantic,
            "c".into(),
            100,
        );
        m.tier = MemoryTier::Short;
        m.entities = entities;
        m
    }

    fn seed_graph() -> Persist {
        let p = Persist::open_in_memory().unwrap();
        // query 实体 q1, 候选 c1(1-hop), c2(2-hop), 远端 c3(3-hop)
        let q1 = EntityNode::new("q1".into(), "Q1".into(), EntityType::Tech);
        let mid = EntityNode::new("mid".into(), "Mid".into(), EntityType::Tech);
        let c1 = EntityNode::new("c1".into(), "C1".into(), EntityType::Tech);
        let c2 = EntityNode::new("c2".into(), "C2".into(), EntityType::Tech);
        let c3 = EntityNode::new("c3".into(), "C3".into(), EntityType::Tech);
        p.put_memory(&mk_item("m-q1", vec![q1])).unwrap();
        p.put_memory(&mk_item("m-mid", vec![mid])).unwrap();
        p.put_memory(&mk_item("m-c1", vec![c1])).unwrap();
        p.put_memory(&mk_item("m-c2", vec![c2])).unwrap();
        p.put_memory(&mk_item("m-c3", vec![c3])).unwrap();
        // q1 -> mid (1-hop), mid -> c1 (so c1 is 2-hop from q1), c1 -> c2 (3-hop), c2 -> c3
        p.put_relation("q1", "mid", "works_on", 1.0, 3, 1).unwrap();
        p.put_relation("mid", "c1", "depends_on", 1.0, 3, 1)
            .unwrap();
        p.put_relation("c1", "c2", "part_of", 1.0, 3, 1).unwrap();
        p.put_relation("c2", "c3", "owns", 1.0, 3, 1).unwrap();
        p
    }

    #[test]
    fn direct_hit_is_one() {
        let p = seed_graph();
        let aff = graph_affinity(&p, &["q1".into()], &["q1".into()], 2).unwrap();
        assert_eq!(aff, 1.0);
    }

    #[test]
    fn two_hop_reachable() {
        let p = seed_graph();
        // q1 -> mid (hop1), mid -> c1 (hop2)
        let aff = graph_affinity(&p, &["q1".into()], &["c1".into()], 2).unwrap();
        assert!((aff - 0.25).abs() < 1e-9, "2-hop = 0.5^2 = 0.25, got {aff}");
    }

    #[test]
    fn one_hop_reachable() {
        let p = seed_graph();
        let aff = graph_affinity(&p, &["q1".into()], &["mid".into()], 2).unwrap();
        assert!((aff - 0.5).abs() < 1e-9, "1-hop = 0.5, got {aff}");
    }

    #[test]
    fn beyond_hop_limit_zero() {
        let p = seed_graph();
        // c2 is 3-hop from q1, hop_limit=2 → 不可达
        let aff = graph_affinity(&p, &["q1".into()], &["c2".into()], 2).unwrap();
        assert_eq!(aff, 0.0, "3-hop beyond limit=2 → 0");
    }

    #[test]
    fn no_relation_zero() {
        let p = Persist::open_in_memory().unwrap();
        let a = EntityNode::new("a".into(), "A".into(), EntityType::Tech);
        let b = EntityNode::new("b".into(), "B".into(), EntityType::Tech);
        p.put_memory(&mk_item("ma", vec![a])).unwrap();
        p.put_memory(&mk_item("mb", vec![b])).unwrap();
        let aff = graph_affinity(&p, &["a".into()], &["b".into()], 2).unwrap();
        assert_eq!(aff, 0.0);
    }

    #[test]
    fn empty_inputs_zero() {
        let p = Persist::open_in_memory().unwrap();
        assert_eq!(graph_affinity(&p, &[], &["x".into()], 2).unwrap(), 0.0);
        assert_eq!(graph_affinity(&p, &["x".into()], &[], 2).unwrap(), 0.0);
    }

    #[test]
    fn pure_direct_hit() {
        let r = vec![];
        let aff = affinity_from_reachability(&["q".into()], &["q".into()], &r);
        assert_eq!(aff, 1.0);
    }

    #[test]
    fn pure_hop_decay() {
        let r = vec![("c1".into(), 1_usize), ("c2".into(), 2_usize)];
        let a1 = affinity_from_reachability(&["q".into()], &["c1".into()], &r);
        let a2 = affinity_from_reachability(&["q".into()], &["c2".into()], &r);
        assert!((a1 - 0.5).abs() < 1e-9);
        assert!((a2 - 0.25).abs() < 1e-9);
    }

    #[test]
    fn pure_max_wins() {
        // 同候选在 hop1 与 hop2 都可达 → 取最短 (max score)
        let r = vec![("c".into(), 2_usize), ("c".into(), 1_usize)];
        let aff = affinity_from_reachability(&["q".into()], &["c".into()], &r);
        assert!((aff - 0.5).abs() < 1e-9, "取最短跳 max score");
    }

    #[test]
    fn pure_unreachable_zero() {
        let r = vec![("c1".into(), 1_usize)];
        let aff = affinity_from_reachability(&["q".into()], &["zzz".into()], &r);
        assert_eq!(aff, 0.0);
    }

    // §1.5: 内存 mock 证明图层单测不依赖 SQLite/Persist::open_in_memory()。
    // 审计 §1.5 故障场景: "任何想用内存图单测 graph_affinity 的测试必须起真实 Persist + SQL 填数据"。
    struct MockGraph {
        reach: std::collections::HashMap<String, Vec<(String, usize)>>,
    }
    impl crate::store::GraphStore for MockGraph {
        fn n_hop_reachable(
            &self,
            start: &str,
            _hop_limit: usize,
        ) -> GraphResult<Vec<(String, usize)>> {
            Ok(self.reach.get(start).cloned().unwrap_or_default())
        }
        fn list_entities_by_type(
            &self,
            _entity_type: &str,
        ) -> GraphResult<Vec<(String, String, String)>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn mock_store_no_sqlite_needed() {
        // q -> mid (hop1), mid -> c1 (hop2): 无 SQLite, 纯内存 HashMap。
        let mut reach = std::collections::HashMap::new();
        reach.insert(
            "q1".into(),
            vec![("mid".into(), 1_usize), ("c1".into(), 2_usize)],
        );
        let mock = MockGraph { reach };
        let aff1 = graph_affinity(&mock, &["q1".into()], &["mid".into()], 2).unwrap();
        assert!((aff1 - 0.5).abs() < 1e-9, "mock hop1 = 0.5");
        let aff2 = graph_affinity(&mock, &["q1".into()], &["c1".into()], 2).unwrap();
        assert!((aff2 - 0.25).abs() < 1e-9, "mock hop2 = 0.25");
    }
}
