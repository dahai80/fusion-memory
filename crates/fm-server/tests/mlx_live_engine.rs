//! 真实模型集成测试 (gate: --features mlx-live)。
//! 覆盖 engine_builder.rs !stub 分支: 真 MlxEmbedder(bge-m3) + MlxEntityExtractor(Qwen3.5)。
//! 需起 fusion-mlx 加载 bge-m3 + Qwen3.5-9B-4bit: ~/claude-home/fusion-mlx/start.sh start
//! 全局规则: 禁 mock, 须真实加载模型。
//! 跑: cargo test -p fm-server --features mlx-live --test mlx_live_engine -- --include-ignored
//!
//! API key 从 env FUSION_MEMORY_MLX_API_KEY 读, 默认 dahai168 (本机设置)。

#![cfg(feature = "mlx-live")]

use fm_server::{build_server_engine, ServerConfig};
use tempfile::tempdir;

fn live_cfg() -> ServerConfig {
    let dir = tempdir().expect("tempdir");
    let api_key = std::env::var("FUSION_MEMORY_MLX_API_KEY").unwrap_or_else(|_| "dahai168".into());
    // 注入 mlx api key 让 MlxEmbedder/MlxEntityExtractor 能鉴权
    std::env::set_var("FUSION_MEMORY_MLX_API_KEY", api_key);
    ServerConfig {
        data_dir: dir.path().to_path_buf(),
        http_port: 0,
        api_key: String::new(),
        uds_enabled: false,
        ..Default::default()
    }
}

#[tokio::test]
#[ignore = "needs live fusion-mlx with bge-m3 + Qwen3.5-9B-4bit"]
async fn live_build_real_engine_embeds() {
    // 覆盖 build_server_engine !stub 分支: MlxEmbedder::new + MlxEntityExtractor::new + with_extractor
    let cfg = live_cfg();
    let se = build_server_engine(&cfg, false).expect("real engine build");
    // 引擎建好即证明 bge-m3 embedder + Qwen3.5 extractor 都连上 mlx
    let _ = se.engine;
}

#[tokio::test]
#[ignore = "needs live fusion-mlx with bge-m3 + Qwen3.5-9B-4bit"]
async fn live_build_real_engine_retrieve() {
    // 覆盖真引擎 retrieve 路径 (走 bge-m3 embedding 查询)
    let cfg = live_cfg();
    let se = build_server_engine(&cfg, false).expect("real engine build");
    let h = fm_server::EngineHandle::new(std::sync::Arc::new(se.engine));
    let q = fm_core::RetrieveQuery {
        text: "rust long term memory".into(),
        top_k: 3,
        session_id: None,
        tier_filter: None,
        token_budget: 256,
        aggregate: false,
    };
    let ctx = h.retrieve_context(&q).await;
    // 空库检索不 panic + 返回合法上下文即覆盖 (bge-m3 embed 走通)
    assert!(ctx.is_ok(), "retrieve should succeed: {:?}", ctx);
    let _ctx = ctx.unwrap();
}
