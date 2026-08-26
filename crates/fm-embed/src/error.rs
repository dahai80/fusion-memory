//! fm-embed 错误类型。对外经 fm-core::MemoryError::Store 暴露。

use thiserror::Error;

pub type EmbedResult<T> = Result<T, EmbedError>;

#[derive(Debug, Error)]
pub enum EmbedError {
    #[error("mlx http error: {0}")]
    Http(String),

    #[error("mlx api error: {0}")]
    Api(String),

    #[error("mlx response parse error: {0}")]
    Parse(String),

    #[error("embedding dimension mismatch: expected {expected}, got {got}")]
    Dimension { expected: usize, got: usize },

    #[error("embedding unavailable: {0}")]
    Unavailable(String),

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl EmbedError {
    pub fn to_memory(self) -> fm_core::MemoryError {
        fm_core::MemoryError::Store(self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_memory_maps_to_store() {
        let e = EmbedError::Unavailable("mlx down".into()).to_memory();
        assert!(matches!(e, fm_core::MemoryError::Store(_)));
        let e2 = EmbedError::Dimension {
            expected: 1024,
            got: 768,
        }
        .to_memory();
        assert!(matches!(e2, fm_core::MemoryError::Store(_)));
    }

    #[test]
    fn serde_variant_from_json() {
        let json_err = serde_json::from_str::<serde_json::Value>("{bad}").unwrap_err();
        let e: EmbedError = json_err.into();
        assert!(matches!(e, EmbedError::Serde(_)));
    }

    #[test]
    fn http_api_parse_distinct() {
        let h = EmbedError::Http("conn".into());
        let a = EmbedError::Api("401".into());
        let p = EmbedError::Parse("bad json".into());
        assert!(!matches!(h, EmbedError::Api(_)));
        assert!(!matches!(a, EmbedError::Http(_)));
        assert!(!matches!(p, EmbedError::Http(_)));
    }
}
