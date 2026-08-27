//! fm-engine: 记忆引擎。PRD §6, §11.1, §11.4, B3/C5。
//!
//! MemoryEngine 实现 FusionMemoryEngine:
//! - commit(turn 级拆分 + Embedder 注入 + 异步抽实体)
//! - retrieve(聚合 interaction_id + 融合评分 α·cos+β·W(t)+γ·graph_aff)
//! - consolidate(W(t) 回收 + Short→Long 晋升 + entities_pending 重抽)
//! - get/delete/audit

pub mod entity_extract;
pub mod redact;
pub mod scoring;

mod engine;

pub use engine::MemoryEngine;
pub use entity_extract::{
    chat_completion, parse_extraction, EntityExtractor, ExtractConfig, ExtractResult,
    MlxEntityExtractor,
};
pub use redact::{redact_enabled_env, redact_text};
pub use scoring::{
    fuse_score, score_candidate, should_promote, should_recycle, weight_of, ALPHA, BETA, GAMMA,
    GRAPH_HOP_LIMIT, THETA_DROP, THETA_PROMOTE,
};
