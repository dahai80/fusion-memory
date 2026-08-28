//! 错误类型。库 crate 用 thiserror（继承 fusion-design Rust 约定）。

use thiserror::Error;

pub type MemoryResult<T> = Result<T, MemoryError>;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("sqlite error: {0}")]
    Sqlite(String),

    #[error("kuzu graph error: {0}")]
    Graph(String),

    #[error("vector store error: {0}")]
    Store(String),

    #[error("embed/llm error: {0}")]
    Embed(String),

    #[error("entity parse error: {0}")]
    EntityParse(String),

    #[error("consolidation error: {0}")]
    Consolidation(String),

    #[error("cluster sync error: {0}")]
    Cluster(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("unsupported operation: {0}")]
    Unsupported(String),

    #[error("auth error: {0}")]
    Auth(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    // v1.0.0 B-1: 静态加密错误 (key 缺失/格式错/解密失败)。永久, 不可重试。
    #[error("encryption error: {0}")]
    Encrypt(String),

    // §2.8: 锁中毒 — 持锁线程 panic, Mutex 永久死。非瞬时, 重试无益, 需重启进程。
    // 旧版被压成 MemoryError::Sqlite("...poisoned") 字符串, 运维误当 sqlite 错误。
    #[error("lock poisoned (prior panic in critical section, restart required)")]
    Poisoned,

    // §2.8/§3.5: 瞬时忙 — SQLITE_BUSY/锁竞争/ sled 压实临时锁。可重试, 退避后可成功。
    // 旧版 SQLITE_BUSY 被 .ok().unwrap_or(0) 吞成全表扫 (§3.5)。
    #[error("resource busy (transient, retry with backoff): {0}")]
    Busy(String),
}

impl MemoryError {
    // §3.1: 是否可重试。JSON-RPC/HTTP 层据此分类返回码, 客户端据此决定重试 vs fail-fast。
    // Busy → 可重试; Poisoned/NotFound/Unsupported/Auth → 永久; 其余未知 → 保守不重试。
    pub fn retryable(&self) -> bool {
        matches!(self, MemoryError::Busy(_))
    }

    // §3.1: 是否"未找到"类永久错误 (id 不存在)。客户端据此 fail-fast 不重试。
    pub fn is_not_found(&self) -> bool {
        matches!(self, MemoryError::NotFound(_))
    }
}
