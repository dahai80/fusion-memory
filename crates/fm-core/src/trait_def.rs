//! 核心引擎 trait。PRD §11.1。
//!
//! 异步签名：commit 同步快路径立返（PRD §6.3），embedding/抽实体 spawn 后台。
//! PyO3 绑定经 py.allow_threads 释放 GIL（C2 修正）。

use async_trait::async_trait;

use crate::context::{FormattedContext, RetrieveQuery};
use crate::error::MemoryResult;
use crate::interaction::Interaction;
use crate::memory::MemoryItem;
use crate::report::{CommitOutcome, ConsolidationReport, MemoryId};

/// fusion-memory 核心引擎 trait。PRD §11.1。
#[async_trait]
pub trait FusionMemoryEngine: Send + Sync {
    /// 写入记忆片段，返回该 interaction 拆出的 turn 级 memory_id 列表（PRD §5.4）。
    /// 同步快路径立返（§6.3），异步 embedding/抽实体不阻塞调用方。
    /// 注意: 失败 turn 静默跳过 (仅记 warn), 此返回值仅含成功 turn id。
    ///       需感知失败 turn 的客户端改用 commit_episodic_memory_detailed (P1-1)。
    async fn commit_episodic_memory(
        &self,
        session_id: &str,
        interaction: &Interaction,
    ) -> MemoryResult<Vec<MemoryId>>;

    /// P1-1: 写入记忆片段, 返回详细结果 (成功/失败 turn 分列)。
    /// 失败 turn 进 failed_turns (embed/insert_vector/persist 任一失败), 客户端可据此重试。
    /// 默认实现退化为 commit_episodic_memory (无失败明细), 生产 MemoryEngine 覆写。
    async fn commit_episodic_memory_detailed(
        &self,
        session_id: &str,
        interaction: &Interaction,
    ) -> MemoryResult<CommitOutcome> {
        let ids = self.commit_episodic_memory(session_id, interaction).await?;
        Ok(CommitOutcome {
            memory_ids: ids,
            failed_turns: Vec::new(),
        })
    }

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

    /// 按 scope (session_id) 批量软删 + 清向量（issue #2 delete_scope RPC）。
    /// 返回被删条数。默认实现 unsupported, 生产 MemoryEngine 覆写。
    async fn delete_scope(&self, _scope: &str) -> MemoryResult<u64> {
        Err(crate::error::MemoryError::Unsupported(
            "delete_scope not implemented for this engine".into(),
        ))
    }

    /// 计数（issue #2 count RPC）。None → 全量, Some(scope) → 按 session_id 过滤。
    /// 默认实现 unsupported, 生产 MemoryEngine 覆写。
    async fn count(&self, _scope: Option<&str>) -> MemoryResult<u64> {
        Err(crate::error::MemoryError::Unsupported(
            "count not implemented for this engine".into(),
        ))
    }

    // #16 多租户隔离 (fusion-gateway #150 Gap1c): 以下方法按权威租户 (gateway
    // X-Fusion-Tenant) 作用域。默认实现退化为非租户版本 (单租户/旧 impl 向后兼容,
    // 不破坏 v1.0 API 冻结), 生产 MemoryEngine 覆写为真租户作用域。tenant="" = 默认租户。

    /// #16: 按 tenant 取单条记忆。租户不匹配 → None (跨租户不可见)。
    /// 默认退化为 get_memory (无租户过滤)。
    async fn get_memory_tenant(&self, id: &str, _tenant: &str) -> MemoryResult<Option<MemoryItem>> {
        self.get_memory(id).await
    }

    /// #16: 按 tenant 软删单条。租户不匹配 → NotFound (跨租户不可删)。
    /// 默认退化为 delete_memory (无租户过滤)。
    async fn delete_memory_tenant(&self, id: &str, _tenant: &str) -> MemoryResult<()> {
        self.delete_memory(id).await
    }

    /// #16: 按 tenant + scope (session_id) 批量软删。默认退化为 delete_scope。
    async fn delete_scope_tenant(&self, scope: &str, _tenant: &str) -> MemoryResult<u64> {
        self.delete_scope(scope).await
    }

    /// #16: 按 tenant + scope 计数。默认退化为 count。
    async fn count_tenant(&self, scope: Option<&str>, _tenant: &str) -> MemoryResult<u64> {
        self.count(scope).await
    }
}
