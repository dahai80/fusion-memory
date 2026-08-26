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
