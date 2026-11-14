//! fm-core: fusion-memory 核心数据结构 + trait 定义。
//!
//! 零业务依赖，仅 serde/thiserror/ulid。供所有 crate 共享类型。
//! 数据结构依据 PRD §5（刷新版），trait 依据 §11.1。

pub mod browse;
pub mod context;
pub mod entity;
pub mod error;
pub mod interaction;
pub mod memory;
pub mod namespace;
pub mod report;
pub mod share;
pub mod trait_def;

pub use browse::{AuditEntry, ListQuery, MemoryRevision, MergeEntry, ProvenanceReport};
pub use context::{ContextBlock, FormattedContext, RetrieveQuery};
pub use entity::{EntityNode, EntityType};
pub use error::{MemoryError, MemoryResult};
pub use interaction::{Interaction, ToolCall, Turn};
pub use memory::{
    AdmissionRecord, MemoryItem, MemoryTier, MemoryType, SafetyAuthority, SafetyPersistence,
    SafetyScope,
};
pub use namespace::NamespaceFilter;
pub use report::{
    CommitOutcome, ConsolidationFailure, ConsolidationReport, MemoryId, SleepReport, TurnFailure,
};
pub use share::{RevokeFilter, ShareGrant};
pub use trait_def::FusionMemoryEngine;
