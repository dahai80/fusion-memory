//! Interaction（写入输入 + 检索聚合视图）+ Turn + ToolCall。PRD §5.4, §5.4.1。

use serde::{Deserialize, Serialize};

/// 完整对话。双重身份：(a) 写入时消费方输入；(b) 检索时聚合 turn 还原的视图。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interaction {
    pub id: String,
    pub session_id: String,
    pub turns: Vec<Turn>,
    pub timestamp: u64,
    /// 消费方附加元数据，schema 约束见 InteractionMetadata（PRD §5.4.1）。
    pub metadata: serde_json::Value,
}

impl Interaction {
    pub fn metadata_typed(&self) -> InteractionMetadata {
        serde_json::from_value(self.metadata.clone()).unwrap_or_default()
    }
}

/// 单轮对话。user + assistant + tool_calls。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub turn_idx: u32,
    pub user_message: String,
    pub assistant_message: String,
    pub tool_calls: Vec<ToolCall>,
}

/// 工具调用。行为模式来源。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub args: serde_json::Value,
    pub result_summary: String,
}

/// metadata 约束字段（PRD §5.4.1, 审计 §D-§5 修正）。
/// 未声明字段保留但检索时不索引。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InteractionMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_names: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_default_empty() {
        let m = InteractionMetadata::default();
        assert!(m.project_path.is_none());
        assert!(m.tool_names.is_none());
    }

    #[test]
    fn metadata_roundtrip() {
        let m = InteractionMetadata {
            project_path: Some("/x".into()),
            model_name: Some("qwen".into()),
            agent_type: None,
            tool_names: Some(vec!["grep".into()]),
            node_id: None,
        };
        let v = serde_json::to_value(&m).unwrap();
        let back: InteractionMetadata = serde_json::from_value(v).unwrap();
        assert_eq!(back.project_path.as_deref(), Some("/x"));
        assert_eq!(back.tool_names.as_deref(), Some(&["grep".to_string()][..]));
    }

    #[test]
    fn metadata_typed_from_interaction() {
        let ix = Interaction {
            id: "ix".into(),
            session_id: "s".into(),
            turns: vec![],
            timestamp: 0,
            metadata: serde_json::json!({"project_path": "/p"}),
        };
        assert_eq!(ix.metadata_typed().project_path.as_deref(), Some("/p"));
    }
}
