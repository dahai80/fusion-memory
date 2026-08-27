//! persist 错误类型。对外经 `MemoryError::Sqlite`。

use thiserror::Error;

pub type PersistResult<T> = Result<T, PersistError>;

#[derive(Debug, Error)]
pub enum PersistError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    // P1: Mutex poison — 持锁线程 panic 后续不再 panic 放大, 上抛 Err 让调用方决策。
    #[error("persist conn lock poisoned (prior panic in critical section)")]
    Poisoned,
}

impl PersistError {
    pub fn to_memory(self) -> fm_core::MemoryError {
        match self {
            PersistError::Serde(e) => fm_core::MemoryError::Serde(e),
            PersistError::Io(e) => fm_core::MemoryError::Io(e),
            PersistError::Sqlite(e) => fm_core::MemoryError::Sqlite(e.to_string()),
            PersistError::Poisoned => {
                fm_core::MemoryError::Sqlite("persist conn lock poisoned".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_to_memory_passthrough() {
        let e: PersistError = serde_json::from_str::<serde_json::Value>("{bad}")
            .unwrap_err()
            .into();
        let m = e.to_memory();
        assert!(matches!(m, fm_core::MemoryError::Serde(_)));
    }

    #[test]
    fn io_to_memory_passthrough() {
        let e: PersistError = std::fs::read("/nonexistent-xyz-123").unwrap_err().into();
        let m = e.to_memory();
        assert!(matches!(m, fm_core::MemoryError::Io(_)));
    }

    #[test]
    fn sqlite_to_memory_string() {
        let e = PersistError::Sqlite(rusqlite::Error::InvalidQuery);
        let m = e.to_memory();
        assert!(matches!(m, fm_core::MemoryError::Sqlite(_)));
    }
}
