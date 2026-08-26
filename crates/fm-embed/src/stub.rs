//! StubEmbedder: 确定性 FNV hash embedding。PRD §3.3, M1 fallback。
//!
//! 离线/测试/真模型不可用降级用。同 content → 同向量 (可测可验聚合)。
//! 非真实语义, 仅保证确定性与归一化。

use async_trait::async_trait;
use tracing::debug;

use crate::cache::fnv1a_64;
use crate::{EmbedResult, Embedder};

pub struct StubEmbedder {
    dim: usize,
}

impl StubEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }
}

/// 确定性 hash embedding (M1 逻辑, 搬自 fm-engine/src/embed.rs)。
/// 按 dim 维分桶累加 4 字节滑窗哈希, L2 归一化。
pub fn stub_embed(text: &str, dim: usize) -> Vec<f32> {
    let bytes = text.as_bytes();
    let mut v = vec![0.0f32; dim];
    let window = 4usize;
    let mut i = 0;
    while i < bytes.len() {
        let end = (i + window).min(bytes.len());
        let bucket = (fnv1a_64(&bytes[i..end]) as usize) % dim;
        let sign_seed = fnv1a_64(&[bytes[i], end as u8, (end >> 8) as u8]);
        let val = ((sign_seed % 1000) as f32) / 100.0 - 5.0;
        v[bucket] += val;
        i = end;
    }
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    } else {
        v[0] = 1.0;
    }
    v
}

/// ulid 字符串 → u64 vector_id (FNV-1a)。store.insert_vector 的 id。
pub fn vector_id_from_ulid(ulid_str: &str) -> u64 {
    fnv1a_64(ulid_str.as_bytes())
}

#[async_trait]
impl Embedder for StubEmbedder {
    async fn embed(&self, text: &str) -> EmbedResult<Vec<f32>> {
        let v = stub_embed(text, self.dim);
        debug!(dim = self.dim, len = text.len(), "stub embed");
        Ok(v)
    }

    fn dimension(&self) -> usize {
        self.dim
    }

    fn is_live(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_same_text_same_vec() {
        assert_eq!(stub_embed("hello world", 8), stub_embed("hello world", 8));
    }

    #[test]
    fn different_text_different_vec() {
        assert_ne!(stub_embed("hello world", 8), stub_embed("goodbye world", 8));
    }

    #[test]
    fn normalized_unit() {
        let v = stub_embed("some longer text content for embedding test", 16);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "norm={norm}");
    }

    #[test]
    fn empty_text_safe() {
        let v = stub_embed("", 8);
        assert_eq!(v.len(), 8);
    }

    #[test]
    fn vector_id_stable() {
        assert_eq!(
            vector_id_from_ulid("01H8TEST"),
            vector_id_from_ulid("01H8TEST")
        );
        assert_ne!(vector_id_from_ulid("01H8A"), vector_id_from_ulid("01H8B"));
    }

    #[tokio::test]
    async fn embedder_trait_dim_and_not_live() {
        let e = StubEmbedder::new(16);
        assert_eq!(e.dimension(), 16);
        assert!(!e.is_live());
        let v = e.embed("hi").await.unwrap();
        assert_eq!(v.len(), 16);
    }
}
