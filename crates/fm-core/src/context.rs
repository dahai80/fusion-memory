//! 检索输出 + 检索查询。PRD §5.5, §11.1。

use serde::{Deserialize, Serialize};

use crate::interaction::Turn;
use crate::memory::{MemoryTier, MemoryType};

/// 检索查询参数。PRD §11.1。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrieveQuery {
    pub text: String,
    /// turn 级 top_k（聚合前）。
    pub top_k: usize,
    pub session_id: Option<String>,
    pub tier_filter: Option<Vec<MemoryTier>>,
    /// 压缩目标 token 数（Qwen tokenizer 实算，C4 修正）。
    pub token_budget: usize,
    /// 默认 true: 按 interaction_id 聚合还原完整对话。
    pub aggregate: bool,
}

impl RetrieveQuery {
    pub fn new(text: impl Into<String>, top_k: usize, token_budget: usize) -> Self {
        Self {
            text: text.into(),
            top_k,
            session_id: None,
            tier_filter: None,
            token_budget,
            aggregate: true,
        }
    }
}

/// 检索输出。注入消费方 prompt。PRD §5.5。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormattedContext {
    pub blocks: Vec<ContextBlock>,
    /// 经 Qwen tokenizer 实算（C4 修正），供消费方截断。
    pub total_tokens: usize,
}

/// 上下文块（聚合后的 Interaction 视图，含多 Turn）。PRD §5.5。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBlock {
    pub interaction_id: String,
    pub turns: Vec<Turn>,
    pub memory_type: MemoryType,
    pub turns_text: String,
    pub score: f64,
    pub source_entities: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_defaults() {
        let q = RetrieveQuery::new("hi", 10, 2048);
        assert_eq!(q.top_k, 10);
        assert!(q.aggregate);
        assert!(q.session_id.is_none());
    }
}
