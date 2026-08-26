//! store 内部错误类型。对外通过 `MemoryError::Store` 暴露。

use thiserror::Error;

pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sled kv error: {0}")]
    Sled(String),

    #[error("hnsw index error: {0}")]
    Hnsw(String),

    #[error("dimension mismatch: expected {expected}, got {got}")]
    Dimension { expected: usize, got: usize },

    #[error("vector not found: {0}")]
    VectorNotFound(u64),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl StoreError {
    pub fn to_memory(self) -> fm_core::MemoryError {
        match self {
            StoreError::Serde(e) => fm_core::MemoryError::Serde(e),
            StoreError::Io(e) => fm_core::MemoryError::Io(e),
            other => fm_core::MemoryError::Store(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_to_memory_passthrough() {
        let e: StoreError = serde_json::from_str::<serde_json::Value>("{bad}")
            .unwrap_err()
            .into();
        assert!(matches!(e.to_memory(), fm_core::MemoryError::Serde(_)));
    }

    #[test]
    fn io_to_memory_passthrough() {
        let e: StoreError = std::fs::read("/nonexistent-xyz-123").unwrap_err().into();
        assert!(matches!(e.to_memory(), fm_core::MemoryError::Io(_)));
    }

    #[test]
    fn store_variants_map_to_string() {
        let s = StoreError::Sled("io".into()).to_memory();
        assert!(matches!(s, fm_core::MemoryError::Store(_)));
        let h = StoreError::Hnsw("idx".into()).to_memory();
        assert!(matches!(h, fm_core::MemoryError::Store(_)));
        let d = StoreError::Dimension {
            expected: 8,
            got: 4,
        }
        .to_memory();
        assert!(matches!(d, fm_core::MemoryError::Store(_)));
        let n = StoreError::VectorNotFound(7).to_memory();
        assert!(matches!(n, fm_core::MemoryError::Store(_)));
    }
}
