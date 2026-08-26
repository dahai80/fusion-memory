//! fm-embed: embedding 生成。PRD §3.3, §6.3, §6.4, A3 修正。
//!
//! - `Embedder` trait: async embed(text) -> Vec<f32>。
//! - `MlxEmbedder`: 调 fusion-mlx `/v1/embeddings` (bge-m3, 127.0.0.1)。
//!   query embedding LRU+TTL 缓存 (同 query 不重打 mlx, A3)。
//!   mlx 并发信号量 Semaphore(2) 背压 (A3 防雪崩)。
//!   mlx 不可用降级: 返回 EmbedError 不 panic (上层 §6.4 落 warn 返空)。
//! - `StubEmbedder`: 确定性 FNV hash embedding (M1 逻辑搬来, 测试/离线 fallback)。

pub mod cache;
pub mod config;
pub mod error;
pub mod mlx;
pub mod stub;

pub use cache::LruCache;
pub use config::EmbedConfig;
pub use error::{EmbedError, EmbedResult};
pub use mlx::MlxEmbedder;
pub use stub::{stub_embed, vector_id_from_ulid, StubEmbedder};

use async_trait::async_trait;

/// embedding 生成 trait。commit/retrieve 共用。
#[async_trait]
pub trait Embedder: Send + Sync {
    /// 文本 → 向量。
    async fn embed(&self, text: &str) -> EmbedResult<Vec<f32>>;

    /// 向量维度。
    fn dimension(&self) -> usize;

    /// 是否走真实模型 (doctor/降级判定用)。
    fn is_live(&self) -> bool;
}
