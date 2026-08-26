//! MlxEmbedder: 调 fusion-mlx `/v1/embeddings` 真实 bge-m3。PRD §3.3, §6.3, A3。
//!
//! - LRU+TTL 缓存 query embedding (同 query 不重打 mlx)。
//! - Semaphore 并发背压 (全局 ≤2, A3 防雪崩)。
//! - mlx 不可用降级: 返回 EmbedError, 上层落 warn 返空, 不 panic。
//! - 100% offline: 仅连 127.0.0.1 (硬约束 §2.5)。

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::cache::{fnv1a_64, LruCache};
use crate::config::EmbedConfig;
use crate::{EmbedError, EmbedResult, Embedder};

pub struct MlxEmbedder {
    client: reqwest::Client,
    config: EmbedConfig,
    cache: Arc<LruCache>,
    sem: Arc<Semaphore>,
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a str,
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedData>,
}

#[derive(Deserialize)]
struct EmbedData {
    embedding: Vec<f32>,
}

impl MlxEmbedder {
    pub fn new(config: EmbedConfig) -> Result<Self, EmbedError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| EmbedError::Http(format!("client build: {e}")))?;
        let cache = Arc::new(LruCache::new(config.cache_capacity, config.cache_ttl_secs));
        let sem = Arc::new(Semaphore::new(config.mlx_concurrency));
        info!(
            url = %config.mlx_url,
            model = %config.embed_model,
            dim = config.dimension,
            concurrency = config.mlx_concurrency,
            "mlx embedder created"
        );
        Ok(Self {
            client,
            config,
            cache,
            sem,
        })
    }

    pub fn config(&self) -> &EmbedConfig {
        &self.config
    }

    async fn call_mlx(&self, text: &str) -> EmbedResult<Vec<f32>> {
        let _permit = self
            .sem
            .acquire()
            .await
            .map_err(|e| EmbedError::Unavailable(format!("semaphore closed: {e}")))?;
        let url = self.config.embeddings_url();
        let req = EmbedRequest {
            model: &self.config.embed_model,
            input: text,
        };
        let mut builder = self.client.post(&url).json(&req);
        if !self.config.api_key.is_empty() {
            builder = builder.bearer_auth(&self.config.api_key);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| EmbedError::Http(format!("request: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            warn!(status = %status, body = %body.chars().take(200).collect::<String>(), "mlx embeddings non-2xx");
            return Err(EmbedError::Api(format!("status {status}: {body}")));
        }
        let parsed: EmbedResponse = resp
            .json()
            .await
            .map_err(|e| EmbedError::Parse(format!("response json: {e}")))?;
        let data = parsed
            .data
            .into_iter()
            .next()
            .ok_or_else(|| EmbedError::Parse("empty data array".into()))?;
        if data.embedding.len() != self.config.dimension {
            return Err(EmbedError::Dimension {
                expected: self.config.dimension,
                got: data.embedding.len(),
            });
        }
        Ok(data.embedding)
    }
}

#[async_trait]
impl Embedder for MlxEmbedder {
    async fn embed(&self, text: &str) -> EmbedResult<Vec<f32>> {
        let key = fnv1a_64(text.as_bytes());
        if let Some(cached) = self.cache.get(key) {
            debug!("embed cache hit");
            return Ok(cached);
        }
        let vec = self.call_mlx(text).await?;
        self.cache.put(key, vec.clone());
        Ok(vec)
    }

    fn dimension(&self) -> usize {
        self.config.dimension
    }

    fn is_live(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> EmbedConfig {
        EmbedConfig {
            dimension: 1024,
            mlx_concurrency: 2,
            api_key: "test-key".into(),
            ..Default::default()
        }
    }

    #[test]
    fn constructor_ok() {
        let e = MlxEmbedder::new(cfg()).unwrap();
        assert_eq!(e.dimension(), 1024);
        assert!(e.is_live());
        assert_eq!(e.config().embed_model, "BAAI/bge-m3");
    }

    #[tokio::test]
    async fn embed_unreachable_returns_http_error() {
        // 指向不存在端口, 应返回 Http 错误不 panic (降级路径)
        let mut c = cfg();
        c.mlx_url = "http://127.0.0.1:1/v1".into();
        c.timeout_secs = 1;
        let e = MlxEmbedder::new(c).unwrap();
        let res = e.embed("hello").await;
        assert!(res.is_err(), "should error on unreachable mlx");
        let err = res.unwrap_err();
        assert!(matches!(err, EmbedError::Http(_)) || matches!(err, EmbedError::Api(_)));
    }

    #[tokio::test]
    async fn embed_no_api_key_still_calls() {
        // 无 key 也发请求 (mlx 可能拒 401, 但不 panic)
        let mut c = cfg();
        c.mlx_url = "http://127.0.0.1:1/v1".into();
        c.api_key = String::new();
        c.timeout_secs = 1;
        let e = MlxEmbedder::new(c).unwrap();
        let res = e.embed("hello").await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn cache_hit_skips_network() {
        // 预填缓存: 第二次 embed 应命中缓存, 不触网 (指向坏端口也不报错)
        let mut c = cfg();
        c.mlx_url = "http://127.0.0.1:1/v1".into();
        c.timeout_secs = 1;
        let e = MlxEmbedder::new(c).unwrap();
        let key = fnv1a_64(b"cached query");
        e.cache.put(key, vec![0.5f32; 1024]);
        let v = e.embed("cached query").await.unwrap();
        assert_eq!(v.len(), 1024);
        assert!((v[0] - 0.5).abs() < 1e-6);
    }

    #[tokio::test]
    async fn semaphore_limits_concurrency() {
        // 并发 5 个 embed, 信号量=2, 都应最终返回 (排队不丢)
        let mut c = cfg();
        c.mlx_url = "http://127.0.0.1:1/v1".into();
        c.mlx_concurrency = 2;
        c.timeout_secs = 1;
        let e = Arc::new(MlxEmbedder::new(c).unwrap());
        let mut handles = Vec::new();
        for i in 0..5 {
            let e = e.clone();
            handles.push(tokio::spawn(async move {
                let _ = e.embed(&format!("q{i}")).await;
            }));
        }
        for h in handles {
            let _ = h.await;
        }
        // 全部完成不 panic 即通过 (降级返回错误也算完成)
    }
}
