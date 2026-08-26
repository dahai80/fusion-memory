//! fm-embed 配置。PRD §12.3。

/// embedding 配置。env 覆盖见 from_env。
#[derive(Debug, Clone)]
pub struct EmbedConfig {
    /// fusion-mlx base url (OpenAI 兼容)。默认 http://127.0.0.1:11434/v1。
    pub mlx_url: String,
    /// 嵌入模型名。默认 BAAI/bge-m3 (dim=1024)。
    pub embed_model: String,
    /// mlx API key (Bearer)。
    pub api_key: String,
    /// 向量维度 (模型决定, bge-m3=1024)。
    pub dimension: usize,
    /// mlx 并发上限 (全局信号量, A3)。默认 2。
    pub mlx_concurrency: usize,
    /// query embedding LRU 缓存容量 (A3)。默认 1024。
    pub cache_capacity: usize,
    /// query embedding 缓存 TTL 秒 (A3)。默认 3600 (1h)。
    pub cache_ttl_secs: u64,
    /// HTTP 超时秒。
    pub timeout_secs: u64,
}

impl Default for EmbedConfig {
    fn default() -> Self {
        Self {
            mlx_url: "http://127.0.0.1:11434/v1".into(),
            embed_model: "BAAI/bge-m3".into(),
            api_key: String::new(),
            dimension: 1024,
            mlx_concurrency: 2,
            cache_capacity: 1024,
            cache_ttl_secs: 3600,
            timeout_secs: 30,
        }
    }
}

impl EmbedConfig {
    /// 从 env 读覆盖 (PRD §12.3)。
    pub fn from_env() -> Self {
        let mut c = Self::default();
        if let Ok(v) = std::env::var("FUSION_MLX_URL") {
            c.mlx_url = v;
        }
        if let Ok(v) = std::env::var("FUSION_MEMORY_EMBED_MODEL") {
            c.embed_model = v;
        }
        if let Ok(v) = std::env::var("FUSION_MEMORY_MLX_API_KEY") {
            c.api_key = v;
        }
        if let Ok(v) = std::env::var("FUSION_MEMORY_MLX_CONCURRENCY") {
            if let Ok(n) = v.parse::<usize>() {
                if n > 0 {
                    c.mlx_concurrency = n;
                }
            }
        }
        c
    }

    /// embeddings endpoint URL。
    pub fn embeddings_url(&self) -> String {
        format!("{}/embeddings", self.mlx_url.trim_end_matches('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = EmbedConfig::default();
        assert_eq!(c.dimension, 1024);
        assert_eq!(c.mlx_concurrency, 2);
        assert_eq!(c.cache_capacity, 1024);
        assert_eq!(c.embed_model, "BAAI/bge-m3");
    }

    #[test]
    fn embeddings_url_no_double_slash() {
        let c = EmbedConfig {
            mlx_url: "http://127.0.0.1:11434/v1/".into(),
            ..Default::default()
        };
        assert_eq!(c.embeddings_url(), "http://127.0.0.1:11434/v1/embeddings");
        let c2 = EmbedConfig::default();
        assert_eq!(c2.embeddings_url(), "http://127.0.0.1:11434/v1/embeddings");
    }

    #[test]
    fn from_env_overrides_and_defaults() {
        // 单测试串行 env, 避免并行测试 env 竞争。
        // 1) 全设 → 全覆盖
        std::env::set_var("FUSION_MLX_URL", "http://127.0.0.1:11432/v1");
        std::env::set_var("FUSION_MEMORY_EMBED_MODEL", "BAAI/bge-small");
        std::env::set_var("FUSION_MEMORY_MLX_API_KEY", "k-123");
        std::env::set_var("FUSION_MEMORY_MLX_CONCURRENCY", "4");
        let c = EmbedConfig::from_env();
        assert_eq!(c.mlx_url, "http://127.0.0.1:11432/v1");
        assert_eq!(c.embed_model, "BAAI/bge-small");
        assert_eq!(c.api_key, "k-123");
        assert_eq!(c.mlx_concurrency, 4);
        // 2) 非法 concurrency → 保持默认
        std::env::set_var("FUSION_MEMORY_MLX_CONCURRENCY", "not-a-num");
        assert_eq!(EmbedConfig::from_env().mlx_concurrency, 2, "非法值保持默认");
        std::env::set_var("FUSION_MEMORY_MLX_CONCURRENCY", "0");
        assert_eq!(EmbedConfig::from_env().mlx_concurrency, 2, "0 保持默认");
        // 3) 全 unset → 默认
        std::env::remove_var("FUSION_MLX_URL");
        std::env::remove_var("FUSION_MEMORY_EMBED_MODEL");
        std::env::remove_var("FUSION_MEMORY_MLX_API_KEY");
        std::env::remove_var("FUSION_MEMORY_MLX_CONCURRENCY");
        let d = EmbedConfig::from_env();
        assert_eq!(d.mlx_url, "http://127.0.0.1:11434/v1");
        assert_eq!(d.embed_model, "BAAI/bge-m3");
        assert_eq!(d.api_key, "");
        assert_eq!(d.mlx_concurrency, 2);
    }
}
