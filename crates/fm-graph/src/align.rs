//! 实体对齐: A5 规则优先链。PRD §7.4。
//!
//! 规则严格顺序, 命中即停:
//!   1. normalize(case+whitespace) → 同 type 精确名匹配 → 复用 id (priority=3)
//!   2. alias 字典 → 规范名 → 同 type 精确名匹配 → 复用 id (priority=2)
//!   3. 同 type name/alias 精确命中 (case-insensitive) → 复用 id (priority=1)
//!   4. 向量 fallback: 按 EntityType::merge_threshold 余弦阈值 → 合并最近 (priority=0)
//!
//! 全不中 → 新实体 (priority=-1)。
//!
//! 强约束: 同名异 type 不合并 (DB 查询恒按 entity_type 过滤)。
//! LLM alias 仅候选, 不作判定 (运行期写入 entity.aliases, 不在 align 内决策)。

use fm_core::{EntityNode, EntityType};
use fm_persist::Persist;
use tracing::{debug, info};

use crate::alias_dict::canonical;
use crate::error::GraphResult;

/// 向量回退提供者 (engine 注入, fm-graph 不依赖 fm-embed)。
/// 返回某 entity 的向量 (若有)。align 用其算余弦判合并。
pub trait EntityVectorProvider {
    fn vector_of(&self, entity_id: &str) -> Option<Vec<f32>>;

    /// 余弦相似度; 任一缺向量返回 None。
    fn cosine(&self, a: &str, b: &str) -> Option<f64> {
        let va = self.vector_of(a)?;
        let vb = self.vector_of(b)?;
        cosine_impl(&va, &vb)
    }
}

/// 对齐结果。
#[derive(Debug, Clone, PartialEq)]
pub struct AlignOutcome {
    /// 最终归一化后的实体 id (复用存量 or 新建)。
    pub canonical_id: String,
    /// 最终规范化名 (alias 字典可能改名)。
    pub canonical_name: String,
    /// 命中规则优先级: 3>2>1>0(向量), -1=新建。
    pub rule_priority: i64,
    /// 是否合并进存量实体 (true 时 canonical_id 为存量 id)。
    pub merged: bool,
}

fn normalize_name(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cosine_impl(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let mut dot = 0.0_f64;
    let mut na = 0.0_f64;
    let mut nb = 0.0_f64;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += (*x as f64) * (*y as f64);
        na += (*x as f64) * (*x as f64);
        nb += (*y as f64) * (*y as f64);
    }
    if na < 1e-12 || nb < 1e-12 {
        return Some(0.0);
    }
    Some(dot / (na.sqrt() * nb.sqrt()))
}

/// 规则 1+2: normalize → 精确名匹配 (同 type)。仅返回存量 id (priority 由 caller 定)。
fn match_by_name(
    persist: &Persist,
    name: &str,
    entity_type: EntityType,
) -> GraphResult<Option<String>> {
    let norm = normalize_name(name);
    let rows = persist.list_entities_by_type(entity_type.as_str())?;
    if let Some((id, _ename, _)) = rows.iter().find(|(_, ename, _)| ename == &norm) {
        return Ok(Some(id.clone()));
    }
    Ok(None)
}

/// 规则 3: name/alias 精确命中 (case-insensitive, 同 type)。仅返回存量 id。
fn match_by_existing(
    persist: &Persist,
    name: &str,
    entity_type: EntityType,
) -> GraphResult<Option<String>> {
    let target = normalize_name(name).to_ascii_lowercase();
    let rows = persist.list_entities_by_type(entity_type.as_str())?;
    for (id, ename, aliases_json) in &rows {
        if ename.to_ascii_lowercase() == target {
            return Ok(Some(id.clone()));
        }
        let aliases: Vec<String> = serde_json::from_str(aliases_json).unwrap_or_default();
        for a in &aliases {
            if normalize_name(a).to_ascii_lowercase() == target {
                return Ok(Some(id.clone()));
            }
        }
    }
    Ok(None)
}

/// 规则 4: 向量 fallback。按 EntityType 阈值, 同 type 内取最相似且超阈值者。
fn match_by_vector(
    persist: &Persist,
    provider: &dyn EntityVectorProvider,
    candidate_id: &str,
    entity_type: EntityType,
) -> GraphResult<Option<(String, f64)>> {
    let threshold = match entity_type.merge_threshold() {
        Some(t) => t,
        None => return Ok(None), // User/Preference/Project/Behavior/Goal 禁向量合并
    };
    let candidate_vec = match provider.vector_of(candidate_id) {
        Some(v) => v,
        None => return Ok(None),
    };
    let rows = persist.list_entities_by_type(entity_type.as_str())?;
    let mut best: Option<(String, f64)> = None;
    for (id, _ename, _aliases) in &rows {
        if id.as_str() == candidate_id {
            continue;
        }
        if let Some(other_vec) = provider.vector_of(id) {
            if let Some(sim) = cosine_impl(&candidate_vec, &other_vec) {
                if sim >= threshold {
                    match &best {
                        Some((_, bs)) if *bs >= sim => {}
                        _ => best = Some((id.clone(), sim)),
                    }
                }
            }
        }
    }
    Ok(best)
}

/// 对齐单个候选实体。返回对齐结果 (不写库; 写库由 caller 用 result 处理)。
pub fn align_entity(
    persist: &Persist,
    candidate: &EntityNode,
    provider: Option<&dyn EntityVectorProvider>,
) -> GraphResult<AlignOutcome> {
    let etype = candidate.entity_type;
    // 规则 1: normalize 精确名匹配 (priority=3)
    if let Some(id) = match_by_name(persist, &candidate.name, etype)? {
        debug!(rule = 1, %id, "align rule1 normalize match");
        return Ok(AlignOutcome {
            canonical_id: id,
            canonical_name: candidate.name.clone(),
            rule_priority: 3,
            merged: true,
        });
    }
    // 规则 2: alias 字典 → 规范名 → 精确名匹配 (priority=2)
    if let Some(canon_name) = canonical(&candidate.name) {
        if let Some(id) = match_by_name(persist, &canon_name, etype)? {
            debug!(rule = 2, %id, "align rule2 alias-dict match");
            return Ok(AlignOutcome {
                canonical_id: id,
                canonical_name: canon_name.clone(),
                rule_priority: 2,
                merged: true,
            });
        }
        // alias 改名后无存量, 保留规范名; 用规范名继续走规则 3
        if let Some(id) = match_by_existing(persist, &canon_name, etype)? {
            debug!(rule = 2, %id, "align rule2 alias-dict existing match");
            return Ok(AlignOutcome {
                canonical_id: id,
                canonical_name: canon_name,
                rule_priority: 2,
                merged: true,
            });
        }
    }
    // 规则 3: 同 type name/alias 精确命中 (priority=1)
    if let Some(id) = match_by_existing(persist, &candidate.name, etype)? {
        debug!(rule = 3, %id, "align rule3 existing match");
        return Ok(AlignOutcome {
            canonical_id: id,
            canonical_name: candidate.name.clone(),
            rule_priority: 1,
            merged: true,
        });
    }
    // 规则 4: 向量 fallback (priority=0)
    if let Some(provider) = provider {
        if let Some((id, sim)) = match_by_vector(persist, provider, &candidate.id, etype)? {
            info!(rule = 4, %id, sim, "align rule4 vector fallback merge");
            return Ok(AlignOutcome {
                canonical_id: id,
                canonical_name: candidate.name.clone(),
                rule_priority: 0,
                merged: true,
            });
        }
    }
    // 全不中 → 新实体
    let canon_name = canonical(&candidate.name).unwrap_or_else(|| candidate.name.clone());
    debug!(rule = -1, "align new entity");
    Ok(AlignOutcome {
        canonical_id: candidate.id.clone(),
        canonical_name: canon_name,
        rule_priority: -1,
        merged: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_core::{EntityNode, EntityType, MemoryItem, MemoryTier, MemoryType};
    use fm_persist::Persist;
    use std::collections::HashMap;

    fn mk_item(id: &str, entities: Vec<EntityNode>) -> MemoryItem {
        let mut m = MemoryItem::new_turn_skeleton(
            id.into(),
            "ix".into(),
            0,
            "s".into(),
            MemoryType::Semantic,
            "c".into(),
            100,
        );
        m.tier = MemoryTier::Short;
        m.entities = entities;
        m
    }

    fn seed_rust() -> Persist {
        let p = Persist::open_in_memory().unwrap();
        let rust = EntityNode::new("ent-rust".into(), "Rust".into(), EntityType::Tech);
        p.put_memory(&mk_item("m1", vec![rust])).unwrap();
        p
    }

    #[test]
    fn rule1_normalize_exact_match() {
        let p = seed_rust();
        // "Rust" (exact) → 复用 ent-rust, priority 3
        let cand = EntityNode::new("cand-1".into(), "Rust".into(), EntityType::Tech);
        let out = align_entity(&p, &cand, None).unwrap();
        assert!(out.merged);
        assert_eq!(out.canonical_id, "ent-rust");
        assert_eq!(out.rule_priority, 3);
    }

    #[test]
    fn rule1_whitespace_trim() {
        let p = seed_rust();
        let cand = EntityNode::new("cand-2".into(), "  Rust  ".into(), EntityType::Tech);
        let out = align_entity(&p, &cand, None).unwrap();
        assert!(out.merged);
        assert_eq!(out.canonical_id, "ent-rust");
        assert_eq!(out.rule_priority, 3);
    }

    #[test]
    fn rule2_alias_dict_merge() {
        let p = seed_rust();
        // "rust-lang" → 字典规范名 "Rust" → 精确名匹配 → 复用
        let cand = EntityNode::new("cand-3".into(), "rust-lang".into(), EntityType::Tech);
        let out = align_entity(&p, &cand, None).unwrap();
        assert!(out.merged);
        assert_eq!(out.canonical_id, "ent-rust");
        assert_eq!(out.canonical_name, "Rust");
        assert_eq!(out.rule_priority, 2);
    }

    #[test]
    fn rule3_existing_alias_match() {
        let p = Persist::open_in_memory().unwrap();
        // 存量实体 name="Python", aliases=["py3"]
        let mut py = EntityNode::new("ent-py".into(), "Python".into(), EntityType::Tech);
        py.aliases.push("py3".into());
        p.put_memory(&mk_item("mp", vec![py])).unwrap();
        // 候选 "Py3" → rule1 no (normalize "Py3" != "Python"), rule2 no (字典无 py3)
        // rule3: alias "py3" case-insensitive 命中
        let cand = EntityNode::new("cand-4".into(), "Py3".into(), EntityType::Tech);
        let out = align_entity(&p, &cand, None).unwrap();
        assert!(out.merged);
        assert_eq!(out.canonical_id, "ent-py");
        assert_eq!(out.rule_priority, 1);
    }

    #[test]
    fn same_name_different_type_no_merge() {
        // 强约束: "Rust" Tech vs "Rust" Concept 不合并
        let p = Persist::open_in_memory().unwrap();
        let rust_tech = EntityNode::new("ent-rust-tech".into(), "Rust".into(), EntityType::Tech);
        p.put_memory(&mk_item("mt", vec![rust_tech])).unwrap();
        // 候选同名但 Concept 类型
        let cand = EntityNode::new("cand-5".into(), "Rust".into(), EntityType::Concept);
        let out = align_entity(&p, &cand, None).unwrap();
        assert!(!out.merged, "同名异 type 必须新建不合并");
        assert_eq!(out.rule_priority, -1);
        assert_eq!(out.canonical_id, "cand-5");
    }

    #[test]
    fn no_match_new_entity() {
        let p = seed_rust();
        let cand = EntityNode::new("cand-new".into(), "Zig".into(), EntityType::Tech);
        let out = align_entity(&p, &cand, None).unwrap();
        assert!(!out.merged);
        assert_eq!(out.rule_priority, -1);
        assert_eq!(out.canonical_id, "cand-new");
    }

    // ---- 向量 fallback 测试 ----

    struct FakeVec {
        map: HashMap<String, Vec<f32>>,
    }
    impl EntityVectorProvider for FakeVec {
        fn vector_of(&self, id: &str) -> Option<Vec<f32>> {
            self.map.get(id).cloned()
        }
    }

    #[test]
    fn rule4_vector_merge_tech_threshold() {
        let p = Persist::open_in_memory().unwrap();
        // 存量 "Rust" Tech, 向量 [1,0]
        let rust = EntityNode::new("ent-rust".into(), "Rust".into(), EntityType::Tech);
        p.put_memory(&mk_item("m", vec![rust])).unwrap();
        // 候选 "Rusty" Tech (未入库, 仅 provider 有向量), cos≈0.9999 ≥ 0.95
        let cand = EntityNode::new("cand-vec".into(), "Rusty".into(), EntityType::Tech);
        let mut m = HashMap::new();
        m.insert("ent-rust".into(), vec![1.0_f32, 0.0_f32]);
        m.insert("cand-vec".into(), vec![0.99_f32, 0.01_f32]);
        let prov = FakeVec { map: m };
        let out = align_entity(&p, &cand, Some(&prov)).unwrap();
        assert!(out.merged, "Tech 阈值 0.95, cos≈0.9999 应合并");
        assert_eq!(out.canonical_id, "ent-rust");
        assert_eq!(out.rule_priority, 0);
    }

    #[test]
    fn rule4_vector_below_threshold_no_merge() {
        let p = Persist::open_in_memory().unwrap();
        let rust = EntityNode::new("ent-rust".into(), "Rust".into(), EntityType::Tech);
        p.put_memory(&mk_item("m", vec![rust])).unwrap();
        // 候选 "Kotlin" Tech (未入库), 向量正交 (cos=0 < 0.95)
        let cand = EntityNode::new("cand-vec2".into(), "Kotlin".into(), EntityType::Tech);
        let mut m = HashMap::new();
        m.insert("ent-rust".into(), vec![1.0_f32, 0.0_f32]);
        m.insert("cand-vec2".into(), vec![0.0_f32, 1.0_f32]);
        let prov = FakeVec { map: m };
        let out = align_entity(&p, &cand, Some(&prov)).unwrap();
        assert!(!out.merged, "cos=0 < 0.95 不合并");
        assert_eq!(out.rule_priority, -1);
    }

    #[test]
    fn rule4_user_type_disabled() {
        // User 类型 merge_threshold=None → 禁向量合并
        let p = Persist::open_in_memory().unwrap();
        let user = EntityNode::new("ent-u".into(), "Alice".into(), EntityType::User);
        p.put_memory(&mk_item("m", vec![user])).unwrap();
        let cand = EntityNode::new("cand-u".into(), "Alicia".into(), EntityType::User);
        let mut m = HashMap::new();
        m.insert("ent-u".into(), vec![1.0_f32]);
        m.insert("cand-u".into(), vec![1.0_f32]);
        let prov = FakeVec { map: m };
        let out = align_entity(&p, &cand, Some(&prov)).unwrap();
        assert!(!out.merged, "User 类型禁向量合并");
        assert_eq!(out.rule_priority, -1);
    }

    #[test]
    fn rule4_concept_wider_threshold() {
        // Concept 阈值 0.85 (比 Tech 0.95 宽)
        let p = Persist::open_in_memory().unwrap();
        let c = EntityNode::new("ent-c".into(), "Gravity".into(), EntityType::Concept);
        p.put_memory(&mk_item("m", vec![c])).unwrap();
        let cand = EntityNode::new("cand-c".into(), "Gravitation".into(), EntityType::Concept);
        let mut m = HashMap::new();
        m.insert("ent-c".into(), vec![1.0_f32, 0.0_f32]);
        m.insert("cand-c".into(), vec![0.9_f32, 0.1_f32]); // cos≈0.994 ≥ 0.85
        let prov = FakeVec { map: m };
        let out = align_entity(&p, &cand, Some(&prov)).unwrap();
        assert!(out.merged, "Concept 阈值 0.85 应合并");
        assert_eq!(out.canonical_id, "ent-c");
    }

    #[test]
    fn normalize_name_collapses_whitespace() {
        assert_eq!(normalize_name("  a   b  "), "a b");
        assert_eq!(normalize_name("Rust"), "Rust");
    }

    #[test]
    fn cosine_impl_basic() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![1.0_f32, 0.0];
        assert!((cosine_impl(&a, &b).unwrap() - 1.0).abs() < 1e-9);
        let c = vec![0.0_f32, 1.0];
        assert!((cosine_impl(&a, &c).unwrap() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn cosine_impl_dim_mismatch_none() {
        let a = vec![1.0_f32];
        let b = vec![1.0_f32, 0.0];
        assert!(cosine_impl(&a, &b).is_none());
    }
}
