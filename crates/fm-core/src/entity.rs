//! 实体节点 + 实体类型。PRD §5.3。
//!
//! 实体类型表刻意与 fusion-rag 的 PERSON/ORG/... 不同 ——
//! fusion-memory 关心对话/行为维度，非文档内容维度（PRD §3.4）。

use serde::{Deserialize, Serialize};

/// 实体类型。对齐 Kuzu schema 注释（PRD §9.2, E2-17 修正全集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntityType {
    User,
    Preference,
    Project,
    Tech,
    Behavior,
    Goal,
    Concept,
}

impl EntityType {
    pub fn as_str(self) -> &'static str {
        match self {
            EntityType::User => "User",
            EntityType::Preference => "Preference",
            EntityType::Project => "Project",
            EntityType::Tech => "Tech",
            EntityType::Behavior => "Behavior",
            EntityType::Goal => "Goal",
            EntityType::Concept => "Concept",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "User" => Some(Self::User),
            "Preference" => Some(Self::Preference),
            "Project" => Some(Self::Project),
            "Tech" => Some(Self::Tech),
            "Behavior" => Some(Self::Behavior),
            "Goal" => Some(Self::Goal),
            "Concept" => Some(Self::Concept),
            _ => None,
        }
    }

    /// 向量合并阈值分档（PRD §7.4, A5 修正）。
    /// Tech 类严 ≥0.95，Concept 类宽 ≥0.85，User 类禁向量合并。
    pub fn merge_threshold(self) -> Option<f64> {
        match self {
            EntityType::Tech => Some(0.95),
            EntityType::Concept => Some(0.85),
            _ => None,
        }
    }
}

/// 实体节点（图节点投影）。PRD §5.3。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityNode {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub entity_type: EntityType,
}

impl EntityNode {
    pub fn new(id: String, name: String, entity_type: EntityType) -> Self {
        Self {
            id,
            name,
            aliases: Vec::new(),
            entity_type,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_type_roundtrip() {
        for t in [
            EntityType::User,
            EntityType::Preference,
            EntityType::Project,
            EntityType::Tech,
            EntityType::Behavior,
            EntityType::Goal,
            EntityType::Concept,
        ] {
            assert_eq!(EntityType::parse(t.as_str()), Some(t));
        }
    }

    #[test]
    fn merge_threshold_segmented() {
        assert_eq!(EntityType::Tech.merge_threshold(), Some(0.95));
        assert_eq!(EntityType::Concept.merge_threshold(), Some(0.85));
        assert_eq!(EntityType::User.merge_threshold(), None);
        assert_eq!(EntityType::Project.merge_threshold(), None);
    }
}
