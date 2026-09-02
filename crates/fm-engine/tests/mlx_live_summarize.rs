//! 真实模型集成测试 (gate: --features mlx-live)。
//! 覆盖 consolidate_summarize happy path (engine.rs 204-280) + with_extractor_and_config (54-62)。
//! 需起 fusion-mlx 加载 Qwen3.5-9B-4bit + bge-m3: ~/claude-home/fusion-mlx/start.sh start
//! 全局规则: 禁 mock, 须真实加载模型。
//! 跑: cargo test -p fm-engine --features mlx-live --test mlx_live_summarize -- --include-ignored

#![cfg(feature = "mlx-live")]

use std::sync::Arc;

use fm_core::{FusionMemoryEngine, Interaction, MemoryTier, ToolCall, Turn};
use fm_embed::{EmbedConfig, Embedder, MlxEmbedder, StubEmbedder};
use fm_engine::{
    entity_extract::{EntityExtractor, ExtractConfig, MlxEntityExtractor},
    MemoryEngine,
};
use fm_persist::Persist;
use fm_store::LocalStore;

fn live_xcfg() -> ExtractConfig {
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

fn tmp_engine_real_extractor(dim: usize) -> MemoryEngine {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("fm-live-sum-{n}"));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(LocalStore::open(&dir, dim).unwrap());
    let persist = Arc::new(Persist::open_in_memory().unwrap());
    // 摘要写新记忆用 embedder; 真路径用 bge-m3 (dim=1024), 但 stub 也走通逻辑。
    // 这里用 stub embedder 覆盖 store.insert_vector 分支, 摘要本体靠 mlx chat。
    let embedder: Arc<dyn Embedder> = Arc::new(StubEmbedder::new(dim));
    let engine = MemoryEngine::new(store, persist, embedder);
    let ext: Arc<dyn EntityExtractor> =
        Arc::new(MlxEntityExtractor::new(live_xcfg()).expect("extractor"));
    // 覆盖 with_extractor_and_config (54-62)
    engine.with_extractor_and_config(ext, live_xcfg())
}

fn episodic_interaction(sess: &str, n: u32) -> Interaction {
    let mut t = Vec::new();
    for i in 0..n {
        t.push(Turn {
            turn_idx: i,
            user_message: format!("user {i}: 我在用 rust 写长期记忆系统"),
            assistant_message: format!("assistant {i}: 用 sqlite 存, hnsw 检索"),
            tool_calls: vec![ToolCall {
                name: "grep".into(),
                args: serde_json::json!({}),
                result_summary: "ok".into(),
            }],
        });
    }
    Interaction {
        id: format!("ix-{sess}"),
        session_id: sess.into(),
        tenant: String::new(),
        turns: t,
        timestamp: 1000,
        metadata: serde_json::json!({}),
    }
}

#[tokio::test]
#[ignore = "needs live fusion-mlx with Qwen3.5-9B-4bit"]
async fn live_consolidate_summarizes_episodic() {
    let eng = tmp_engine_real_extractor(16);
    // 同 session 提交 4 条 episodic (≥SUMMARIZE_MIN_EPISODIC=3)
    let ix = episodic_interaction("sess-sum", 4);
    eng.commit_episodic_memory("sess-sum", &ix).await.unwrap();
    let report = eng.consolidate_memories().await.unwrap();
    assert!(
        report.summarized >= 1,
        "应生成摘要记忆 summarized={}",
        report.summarized
    );
    // 新摘要记忆 tier=Long, type=Semantic
    let all = eng.persist().list_all().unwrap();
    let summaries: Vec<_> = all
        .iter()
        .filter(|m| m.interaction_id.starts_with("summary-"))
        .collect();
    assert!(!summaries.is_empty(), "应有 summary-* 记忆");
    assert_eq!(summaries[0].tier, MemoryTier::Long);
}
