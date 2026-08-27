//! 真实模型集成测试 (gate: --features mlx-live)。
//! 需起 fusion-mlx 加载 Qwen3.5-9B-4bit: ~/claude-home/fusion-mlx/start.sh start
//! 全局规则: 禁 mock, 须真实加载模型。
//! 跑: cargo test -p fm-engine --features mlx-live --test mlx_live_extract -- --include-ignored
//!
//! API key 从 env FUSION_MEMORY_MLX_API_KEY 读, 默认 change-me (本机设置)。

#![cfg(feature = "mlx-live")]

use fm_engine::entity_extract::{
    chat_completion, EntityExtractor, ExtractConfig, MlxEntityExtractor,
};

fn live_cfg() -> ExtractConfig {
    let api_key = std::env::var("FUSION_MEMORY_MLX_API_KEY").unwrap_or_else(|_| "change-me".into());
    ExtractConfig {
        mlx_url: std::env::var("FUSION_MLX_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:11434/v1".into()),
        api_key,
        chat_model: std::env::var("FUSION_MEMORY_CHAT_MODEL")
            .unwrap_or_else(|_| "Qwen3.5-9B-4bit".into()),
        timeout_secs: 60,
    }
}

#[tokio::test]
#[ignore = "needs live fusion-mlx with Qwen3.5-9B-4bit"]
async fn live_extractor_construct_and_config() {
    let cfg = live_cfg();
    let ext = MlxEntityExtractor::new(cfg.clone()).expect("extractor");
    assert_eq!(ext.config().chat_model, cfg.chat_model);
    assert_eq!(ext.config().mlx_url, cfg.mlx_url);
}

#[tokio::test]
#[ignore = "needs live fusion-mlx with Qwen3.5-9B-4bit"]
async fn live_extract_finds_entities() {
    let ext = MlxEntityExtractor::new(live_cfg()).expect("extractor");
    let turn = "我在用 Rust 写 fusion-memory 这个项目，目标是做长期记忆图。\
                我偏好用 SQLite 不想引外部 DB。";
    let r = ext.extract(turn).await;
    assert!(r.success, "extract should succeed: {:?}", r.entities);
    // 至少抽到 Rust / fusion-memory / SQLite 之一
    let names: Vec<&str> = r.entities.iter().map(|e| e.name.as_str()).collect();
    let hit = names.iter().any(|n| {
        n.contains("Rust") || n.contains("fusion") || n.contains("SQLite") || n.contains("sqlite")
    });
    assert!(hit, "应抽到已知实体, got: {names:?}");
}

#[tokio::test]
#[ignore = "needs live fusion-mlx with Qwen3.5-9B-4bit"]
async fn live_extract_empty_on_no_entity_text() {
    let ext = MlxEntityExtractor::new(live_cfg()).expect("extractor");
    let r = ext.extract("嗯。啊。好的。").await;
    // 无明确实体 → 成功但可能空 (不强制空, LLM 可能误抽, 只验证不 panic + success 可 false)
    let _ = r.entities;
}

#[tokio::test]
#[ignore = "needs live fusion-mlx with Qwen3.5-9B-4bit"]
async fn live_chat_completion_returns_content() {
    let cfg = live_cfg();
    let out = chat_completion(&cfg, "你只回复两个字符: ok", "请回复").await;
    assert!(out.is_some(), "chat_completion 应有返回");
    let s = out.unwrap();
    assert!(!s.is_empty(), "content 不应为空");
}

#[tokio::test]
#[ignore = "needs live fusion-mlx with Qwen3.5-9B-4bit"]
async fn live_extract_wrong_api_key_returns_failure() {
    let mut cfg = live_cfg();
    cfg.api_key = "wrong-key".into();
    let ext = MlxEntityExtractor::new(cfg).expect("extractor");
    let r = ext.extract("some text about Rust").await;
    // 401 → non-2xx → success=false, entities 空
    assert!(!r.success, "错误 key 应 success=false");
    assert!(r.entities.is_empty(), "失败不应有实体");
}

#[tokio::test]
#[ignore = "needs live fusion-mlx with Qwen3.5-9B-4bit"]
async fn live_extract_unreachable_url_returns_failure() {
    let cfg = ExtractConfig {
        mlx_url: "http://127.0.0.1:1/v1".into(),
        api_key: "change-me".into(),
        chat_model: "Qwen3.5-9B-4bit".into(),
        timeout_secs: 2,
    };
    let ext = MlxEntityExtractor::new(cfg).expect("extractor");
    let r = ext.extract("text").await;
    // 连接拒绝 → send Err → success=false
    assert!(!r.success, "不可达 url 应 success=false");
    assert!(r.entities.is_empty());
}
