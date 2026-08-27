//! fm-graph: 知识图谱对齐 + graph_affinity。PRD §7, §9.2 偏离。
//!
//! Kuzu 无 Rust binding (裁定 2026-08-26), 改 SQLite 递归 CTE。
//! relation 表在 fm-persist schema; 本 crate 只做对齐/亲和计算, 持久化下沉 fm-persist。
//!
//! ## A5 规则优先对齐链 (strict order, 命中即停)
//! 1. case normalize + whitespace trim → 精确名匹配 (rule_priority=3)
//! 2. alias 字典命中 → 归一化 (rule_priority=2)
//! 3. 存量实体 name/alias 精确命中 → 复用 id (rule_priority=1)
//! 4. 向量 fallback: 按 EntityType::merge_threshold 阈值余弦相似
//!    (Tech≥0.95 / Concept≥0.85 / User·Preference·Project·Behavior·Goal 禁向量合并)
//!
//! 强约束: 同名异 type (entity_type 不同) 不合并。
//! LLM alias 仅作候选写入, 不作合并判定。

pub mod affinity;
pub mod alias_dict;
pub mod align;
pub mod error;
// §1.5: 图层存储抽象 trait (解耦 fm-graph 与具体 Persist)。
pub mod store;

pub use affinity::graph_affinity;
pub use alias_dict::{alias_dict, canonical};
pub use align::{align_entity, AlignOutcome};
pub use error::{GraphError, GraphResult};
// §1.5: 导出 GraphStore trait + Persist 适配 (consumer 经 `&dyn GraphStore` 注入)。
pub use store::GraphStore;
