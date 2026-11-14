//! 仓储/遗忘报告 + MemoryId。PRD §5.6, §11.1。

use serde::{Deserialize, Serialize};

/// 记忆 ID 新类型（ULID String）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MemoryId(pub String);

impl MemoryId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for MemoryId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// 遗忘/合并报告。PRD §5.6。失败可见（继承全局规则 Rule 12）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsolidationReport {
    pub dropped: usize,
    pub promoted: usize,
    pub merged: usize,
    pub summarized: usize,
    pub reextracted: usize,
    pub reconciled: usize,
    pub elapsed_ms: u64,
    pub failures: Vec<ConsolidationFailure>,
}

/// P10 #60: sleep-time curation 报告 — 一次离线睡眠周期 = 巩固预计算 + 准入门重放。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SleepReport {
    /// 巩固内核结果 (衰减回收/晋升/合并/摘要/补抽 — 测试时不再需要的预计算)。
    #[serde(default)]
    pub consolidated: ConsolidationReport,
    /// 准入门重放检查的活跃记忆数。
    pub replay_checked: u64,
    /// 重放判定为近重复 (存在更新副本) 而降权的记忆数。
    pub replay_downweighted: u64,
    /// playbook 库内被更新副本取代而降级 (Pinned→Normal) 的条目数。
    pub playbook_demoted: u64,
    /// 整个睡眠周期耗时 (ms) — 离线成本记录。
    pub duration_ms: u64,
}

/// 单次 consolidate 失败项。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationFailure {
    pub memory_id: String,
    pub stage: String,
    pub error: String,
}

/// commit 结果 (P1-1)。成功/失败 turn 分列, 客户端可感知重试失败 turn。
/// 全部 turn 成功 → failed_turns 空; 全部失败 → memory_ids 空 + failed_turns 满 (非 Err)。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommitOutcome {
    /// 成功落库的 turn 级 memory_id 列表 (与 interaction.turns 顺序一致, 跳过的 turn 不含)。
    pub memory_ids: Vec<MemoryId>,
    /// 失败 turn 明细 (embed/insert_vector/persist 失败均记此)。
    pub failed_turns: Vec<TurnFailure>,
    /// #57: 准入门拒绝的 turn 明细 (stage="admission", error=拒绝原因)。未持久化、无向量。
    #[serde(default)]
    pub rejected_turns: Vec<TurnFailure>,
}

/// 单 turn commit 失败明细 (P1-1)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnFailure {
    pub turn_idx: u32,
    /// 失败阶段: "embed" / "insert_vector" / "persist"。
    pub stage: String,
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_default() {
        let r = ConsolidationReport::default();
        assert_eq!(r.dropped, 0);
        assert!(r.failures.is_empty());
    }

    #[test]
    fn memory_id_from_str() {
        let id: MemoryId = "01H8...".to_string().into();
        assert_eq!(id.as_str(), "01H8...");
    }
}
