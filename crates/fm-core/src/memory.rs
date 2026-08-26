//! 记忆条目 + 记忆类型/层级。PRD §5.1, §5.2。
//!
//! 存储最小单元 = 单轮 turn（用户裁定）。一条 turn = 一条 MemoryItem。

use serde::{Deserialize, Serialize};

use crate::entity::EntityNode;

/// 记忆类型：事件性/语义性/程序性。各自衰减时间常数 τ 不同（PRD §5.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryType {
    Episodic,
    Semantic,
    Procedural,
}

impl MemoryType {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryType::Episodic => "Episodic",
            MemoryType::Semantic => "Semantic",
            MemoryType::Procedural => "Procedural",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Episodic" => Some(Self::Episodic),
            "Semantic" => Some(Self::Semantic),
            "Procedural" => Some(Self::Procedural),
            _ => None,
        }
    }

    /// 初始权重 W0（PRD §5.1）。
    pub fn initial_weight(self) -> f64 {
        match self {
            MemoryType::Episodic => 0.6,
            MemoryType::Semantic => 0.8,
            MemoryType::Procedural => 1.0,
        }
    }

    /// 衰减时间常数 τ（秒）。Episodic=1天 / Semantic=30天 / Procedural=90天。
    pub fn tau_seconds(self) -> f64 {
        match self {
            MemoryType::Episodic => 86_400.0,
            MemoryType::Semantic => 2_592_000.0,
            MemoryType::Procedural => 7_776_000.0,
        }
    }
}

/// 三级记忆调度层级。PRD §6。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryTier {
    Working,
    Short,
    Long,
}

impl MemoryTier {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryTier::Working => "working",
            MemoryTier::Short => "short",
            MemoryTier::Long => "long",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "working" => Some(Self::Working),
            "short" => Some(Self::Short),
            "long" => Some(Self::Long),
            _ => None,
        }
    }
}

/// 记忆条目（turn 级最小存储单元）。PRD §5.2 刷新版。
///
/// 关键字段（相对原始 PRD 变更见 PRD §5.2 注释）：
/// - `interaction_id` + `turn_idx`: turn 级存储，聚合检索。
/// - `vector_ref: String`: 统一 ID 体系（C6 修正）。
/// - `weight: f64`: 抗精度损失（B3 修正）。
/// - `tombstone: bool`: 跨库一致删除标记（A1 修正）。
/// - `entities_pending: bool`: 抽取失败待补抽（C5 修正）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: String,
    pub interaction_id: String,
    pub turn_idx: u32,
    pub session_id: String,
    pub memory_type: MemoryType,
    pub tier: MemoryTier,
    pub content: String,
    pub entities: Vec<EntityNode>,
    pub vector_ref: String,
    pub weight: f64,
    pub access_count: u64,
    pub last_accessed_timestamp: u64,
    pub created_timestamp: u64,
    pub provenance: Option<String>,
    pub tombstone: bool,
    pub entities_pending: bool,
}

impl MemoryItem {
    /// 构造 turn 级骨架（commit 同步快路径用，PRD §6.3）。
    /// vector_ref 置空，entities_pending=true，待异步回填。
    pub fn new_turn_skeleton(
        id: String,
        interaction_id: String,
        turn_idx: u32,
        session_id: String,
        memory_type: MemoryType,
        content: String,
        created_timestamp: u64,
    ) -> Self {
        Self {
            id,
            interaction_id,
            turn_idx,
            session_id,
            memory_type,
            tier: MemoryTier::Working,
            content,
            entities: Vec::new(),
            vector_ref: String::new(),
            weight: memory_type.initial_weight(),
            access_count: 0,
            last_accessed_timestamp: created_timestamp,
            created_timestamp,
            provenance: None,
            tombstone: false,
            entities_pending: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_type_weights() {
        assert!((MemoryType::Episodic.initial_weight() - 0.6).abs() < 1e-9);
        assert!((MemoryType::Semantic.initial_weight() - 0.8).abs() < 1e-9);
        assert!((MemoryType::Procedural.initial_weight() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn memory_type_tau_ordering() {
        assert!(MemoryType::Episodic.tau_seconds() < MemoryType::Semantic.tau_seconds());
        assert!(MemoryType::Semantic.tau_seconds() < MemoryType::Procedural.tau_seconds());
    }

    #[test]
    fn tier_roundtrip() {
        for t in [MemoryTier::Working, MemoryTier::Short, MemoryTier::Long] {
            assert_eq!(MemoryTier::parse(t.as_str()), Some(t));
        }
        assert_eq!(MemoryTier::parse("bogus"), None);
    }

    #[test]
    fn skeleton_defaults() {
        let m = MemoryItem::new_turn_skeleton(
            "id".into(),
            "ix".into(),
            0,
            "s".into(),
            MemoryType::Semantic,
            "hi".into(),
            1000,
        );
        assert_eq!(m.tier, MemoryTier::Working);
        assert!(m.entities_pending);
        assert!(m.vector_ref.is_empty());
        assert!(!m.tombstone);
        assert!(m.entities.is_empty());
        assert!((m.weight - 0.8).abs() < 1e-9);
    }
}
