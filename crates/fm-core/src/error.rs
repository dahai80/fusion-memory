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

    #[error("auth error: {0}")]
    Auth(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}
