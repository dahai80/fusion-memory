//! 真实模型集成测试 (gate: --features mlx-live)。
//! 需起 fusion-mlx 加载 bge-m3: ~/claude-home/fusion-mlx/start.sh start
//! 全局规则: 禁 mock, 须真实加载模型。
//! 跑: cargo test -p fm-embed --features mlx-live --test mlx_live -- --include-ignored
//!
//! API key 从 env FUSION_MEMORY_MLX_API_KEY 读, 默认 dahai168 (本机设置)。

#![cfg(feature = "mlx-live")]

use fm_embed::{EmbedConfig, Embedder, MlxEmbedder};

fn live_cfg() -> EmbedConfig {
    let mut c = EmbedConfig::from_env();
    if c.api_key.is_empty() {
        c.api_key =
            std::env::var("FUSION_MEMORY_MLX_API_KEY").unwrap_or_else(|_| "dahai168".into());
    }
    c
}

#[tokio::test]
#[ignore = "needs live fusion-mlx with bge-m3"]
async fn live_embed_dim_1024() {
    let e = MlxEmbedder::new(live_cfg()).expect("embedder");
    let v = e
        .embed("hello world from fusion-memory test")
        .await
        .expect("embed ok");
    assert_eq!(v.len(), 1024, "bge-m3 dim must be 1024");
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!(
        (norm - 1.0).abs() < 1e-3,
        "bge-m3 output normalized, norm={norm}"
    );
}

#[tokio::test]
#[ignore = "needs live fusion-mlx with bge-m3"]
async fn live_embed_semantic_similarity() {
    let e = MlxEmbedder::new(live_cfg()).expect("embedder");
    let a = e.embed("Rust cargo build error").await.unwrap();
    let b = e
        .embed("rust compilation failure with cargo")
        .await
        .unwrap();
    let c = e.embed("I like eating apples for breakfast").await.unwrap();
    let sim_ab = fm_similarity::cosine(&a, &b).unwrap();
    let sim_ac = fm_similarity::cosine(&a, &c).unwrap();
    assert!(
        sim_ab > sim_ac,
        "semantic相近应高于无关: ab={sim_ab} ac={sim_ac}"
    );
}

#[tokio::test]
#[ignore = "needs live fusion-mlx with bge-m3"]
async fn live_cache_hit_no_duplicate_call() {
    let e = MlxEmbedder::new(live_cfg()).expect("embedder");
    let v1 = e.embed("cache me please").await.unwrap();
    let v2 = e.embed("cache me please").await.unwrap();
    assert_eq!(v1, v2, "cache hit should return identical vector");
}

#[tokio::test]
#[ignore = "needs live fusion-mlx with bge-m3"]
async fn live_batch_consistency() {
    // 多条文本维度一致
    let e = MlxEmbedder::new(live_cfg()).expect("embedder");
    for text in [
        "short",
        "a longer sentence about rust programming",
        "中文嵌入测试内容",
    ] {
        let v = e.embed(text).await.unwrap();
        assert_eq!(v.len(), 1024, "dim for {text:?}");
    }
}
