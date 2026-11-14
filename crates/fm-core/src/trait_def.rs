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
            rejected_turns: Vec::new(),
        })
    }

    /// 检索并组装记忆上下文（聚合后注入 prompt）。PRD §6.4。
    async fn retrieve_context(&self, query: &RetrieveQuery) -> MemoryResult<FormattedContext>;

    /// 触发后台遗忘与合并（nightly cron，增量 + saga，PRD §7.3）。
    async fn consolidate_memories(&self) -> MemoryResult<ConsolidationReport>;

    /// P10 #60: sleep-time curation — 离线预计算巩固 (摘要/实体刷新/合并) + 准入门重放
    /// (近重复降权 / playbook 降级), 结果落库供冷启动消费者直接命中。幂等: 重放已标记
    /// 的记忆跳过。幂等 + 无交互请求在飞。默认 unsupported, 生产 MemoryEngine 覆写。
    async fn sleep_curation(&self) -> MemoryResult<crate::report::SleepReport> {
        Err(crate::error::MemoryError::Unsupported(
            "sleep-time curation not implemented for this engine".into(),
        ))
    }

    /// issue #21: 按作用域命名空间调度 (仅该 namespace/key 可见域内的记忆参与回收/晋升/合并/摘要)。
    /// 不推进全局 consolidate 水位 (consolidation_log 不写) — 作用域运行是补跑, 不能吞掉
    /// 其他 namespace 的待处理变更。默认实现 unsupported, 生产 MemoryEngine 覆写。
    async fn consolidate_memories_scoped(
        &self,
        _namespace: &str,
        _namespace_key: &str,
    ) -> MemoryResult<ConsolidationReport> {
        Err(crate::error::MemoryError::Unsupported(
            "namespace-scoped consolidation not implemented for this engine".into(),
        ))
    }

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

    // #33 web surface (v1.5.0): browse/manage APIs for the web UI. Default impls
    // unsupported (same pattern as delete_scope/count); production MemoryEngine
    // overrides. tenant="" = default tenant.

    /// #33: paged browse over memory items (created_timestamp ASC), optional
    /// memory_type / namespace filters, optional tombstoned inclusion.
    async fn list_memories(
        &self,
        _q: &crate::browse::ListQuery,
        _tenant: &str,
    ) -> MemoryResult<Vec<MemoryItem>> {
        Err(crate::error::MemoryError::Unsupported(
            "list_memories not implemented for this engine".into(),
        ))
    }

    /// #64 T2: distinct namespace_key values per tenant + tier (team anchors
    /// enumeration for agent-studio). Returns keys only, never content.
    /// Empty/unknown tier → Unsupported-style error (fail-closed).
    async fn list_namespaces(&self, _tier: &str, _tenant: &str) -> MemoryResult<Vec<String>> {
        Err(crate::error::MemoryError::Unsupported(
            "list_namespaces not implemented for this engine".into(),
        ))
    }

    /// #33: latest audit_log entries (id DESC), capped by `limit`.
    async fn list_audit_log(&self, _limit: usize) -> MemoryResult<Vec<crate::browse::AuditEntry>> {
        Err(crate::error::MemoryError::Unsupported(
            "list_audit_log not implemented for this engine".into(),
        ))
    }

    /// #33: full merge_log (id DESC) for the merge console.
    async fn list_merges(&self) -> MemoryResult<Vec<crate::browse::MergeEntry>> {
        Err(crate::error::MemoryError::Unsupported(
            "list_merges not implemented for this engine".into(),
        ))
    }

    /// #33: undo one merge by merge_log id (source un-tombstoned, log row removed).
    /// Returns whether the merge existed.
    async fn unmerge(&self, _merge_id: u64) -> MemoryResult<bool> {
        Err(crate::error::MemoryError::Unsupported(
            "unmerge not implemented for this engine".into(),
        ))
    }

    /// #33: cross-store reconciliation (SQLite ids ↔ store vectors); dangling
    /// rows land in the reconcile report, tombstones physically deleted.
    /// Returns reconciled row count.
    async fn reconcile(&self) -> MemoryResult<usize> {
        Err(crate::error::MemoryError::Unsupported(
            "reconcile not implemented for this engine".into(),
        ))
    }

    /// M6 #45: provenance lookup — a memory's own lineage (sources + confidence)
    /// plus reverse citations (items whose `sources` contain this id).
    /// Powers the web console provenance query (poisoning governance surface).
    async fn provenance(
        &self,
        _id: &str,
        _tenant: &str,
    ) -> MemoryResult<crate::browse::ProvenanceReport> {
        Err(crate::error::MemoryError::Unsupported(
            "provenance not implemented for this engine".into(),
        ))
    }

    /// M8 #49: content revision history for one memory (newest first).
    /// Additive (v1.8.0, default Unsupported — same pattern as provenance).
    async fn list_revisions(
        &self,
        _id: &str,
        _tenant: &str,
    ) -> MemoryResult<Vec<crate::browse::MemoryRevision>> {
        Err(crate::error::MemoryError::Unsupported(
            "list_revisions not implemented for this engine".into(),
        ))
    }

    // v1.10.0 #57/#58: 准入门撤销 + agent 共享授权 + playbook 发布/采纳。
    // 全部默认 Unsupported (v1.2.0 tenant 模式), 生产 MemoryEngine 覆写。

    /// #57: revoke-by-metadata — 按 scope/authority/persistence/session/namespace 组合
    /// tombstone 匹配记忆 (AND 语义), 返回撤销条数。空过滤器 fail-closed 拒绝。
    async fn revoke_by_meta(
        &self,
        _filter: &crate::share::RevokeFilter,
        _tenant: &str,
    ) -> MemoryResult<u64> {
        Err(crate::error::MemoryError::Unsupported(
            "revoke_by_meta not implemented for this engine".into(),
        ))
    }

    /// #58: 授予 grantee 读 grantor agent 命名空间的授权 (幂等)。
    async fn grant_access(&self, _grant: &crate::share::ShareGrant) -> MemoryResult<()> {
        Err(crate::error::MemoryError::Unsupported(
            "grant_access not implemented for this engine".into(),
        ))
    }

    /// #58: 撤销读授权, 返回删除行数。
    async fn revoke_access(
        &self,
        _grantor_key: &str,
        _grantee_key: &str,
        _namespace: &str,
    ) -> MemoryResult<u64> {
        Err(crate::error::MemoryError::Unsupported(
            "revoke_access not implemented for this engine".into(),
        ))
    }

    /// #58: 列 grantee 收到的授权。
    async fn list_grants(
        &self,
        _grantee_key: &str,
        _namespace: &str,
    ) -> MemoryResult<Vec<crate::share::ShareGrant>> {
        Err(crate::error::MemoryError::Unsupported(
            "list_grants not implemented for this engine".into(),
        ))
    }

    /// #58: 发布 namespace-local 洞见进共享 playbook 仓库 (authority=team, pinned)。
    /// 返回 playbook 条目 id。
    async fn publish_playbook(
        &self,
        _memory_id: &str,
        _publisher_key: &str,
        _tenant: &str,
    ) -> MemoryResult<String> {
        Err(crate::error::MemoryError::Unsupported(
            "publish_playbook not implemented for this engine".into(),
        ))
    }

    /// #58: 采纳 playbook 条目进消费方命名空间 — 过 #57 准入门 (近重复拒绝,
    /// 无盲传播)。Ok(Some(id)) = 已入库; Ok(None) = 门拒绝 (裁决走 audit)。
    async fn adopt_playbook(
        &self,
        _playbook_id: &str,
        _consumer_namespace: &str,
        _consumer_key: &str,
        _tenant: &str,
    ) -> MemoryResult<Option<String>> {
        Err(crate::error::MemoryError::Unsupported(
            "adopt_playbook not implemented for this engine".into(),
        ))
    }
}
