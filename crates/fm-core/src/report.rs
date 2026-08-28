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
