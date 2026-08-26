//! 核心引擎 trait。PRD §11.1。
//!
//! 异步签名：commit 同步快路径立返（PRD §6.3），embedding/抽实体 spawn 后台。
//! PyO3 绑定经 py.allow_threads 释放 GIL（C2 修正）。

use async_trait::async_trait;

use crate::context::{FormattedContext, RetrieveQuery};
use crate::error::MemoryResult;
use crate::interaction::Interaction;
use crate::memory::MemoryItem;
use crate::report::{ConsolidationReport, MemoryId};

/// fusion-memory 核心引擎 trait。PRD §11.1。
#[async_trait]
pub trait FusionMemoryEngine: Send + Sync {
    /// 写入记忆片段，返回该 interaction 拆出的 turn 级 memory_id 列表（PRD §5.4）。
    /// 同步快路径立返（§6.3），异步 embedding/抽实体不阻塞调用方。
    async fn commit_episodic_memory(
        &self,
        session_id: &str,
        interaction: &Interaction,
    ) -> MemoryResult<Vec<MemoryId>>;

    /// 检索并组装记忆上下文（聚合后注入 prompt）。PRD §6.4。
    async fn retrieve_context(&self, query: &RetrieveQuery) -> MemoryResult<FormattedContext>;

    /// 触发后台遗忘与合并（nightly cron，增量 + saga，PRD §7.3）。
    async fn consolidate_memories(&self) -> MemoryResult<ConsolidationReport>;

    /// 取单条记忆。
    async fn get_memory(&self, id: &str) -> MemoryResult<Option<MemoryItem>>;

    /// 删除记忆：tombstone 软删（PRD §8.4），非立即物理删。
    async fn delete_memory(&self, id: &str) -> MemoryResult<()>;

    /// 审计接口：供 fusion-guard 查询含某实体的记忆（PRD §10.4）。
    async fn audit_memory_access(&self, entity_ids: &[String]) -> MemoryResult<Vec<MemoryItem>>;
}
