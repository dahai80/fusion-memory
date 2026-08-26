//! fm-graph 错误类型。

use thiserror::Error;

pub type GraphResult<T> = Result<T, GraphError>;

#[derive(Debug, Error)]
pub enum GraphError {
    #[error("persist error: {0}")]
    Persist(#[from] fm_persist::PersistError),

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}
