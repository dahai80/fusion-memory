//! §13.2 live perf 基线: 真 fusion-mlx bge-m3 (dim=1024) embedder 下 retrieve 延迟。
//! 关闭 RC 已知限制 #1 (Perf baseline = StubEmbedder) —— 补真模型 HTTP 延迟数据。
//!
//! gate: --features mlx-live。需起 fusion-mlx 加载 bge-m3。
//!   ~/claude-home/fusion-mlx/start.sh start
//!   (standalone 须 FUSION_ROUTE_WARN_ONLY=true + --api-key <key>)
//! 跑: cargo bench -p fm-engine --features mlx-live --bench retrieve_bench_live
//!
//! 与 retrieve_bench (StubEmbedder dim=64) 区别: embedder 走真 HTTP /v1/embeddings,
//! 含 mlx 推理 + 网络往返延迟。retrieve_context 内 query embed 是热路径 (每查 1 次),
//! 故本 bench 测的是端到端 retrieve (embed + 向量检索 + context 组装), 非 100% 索引层。
//! LRU 缓存命中 (同 query) 跳 HTTP —— warmup 后单条测若 query 相同则走缓存, 反映生产
//! 重复 query 场景; 用唯一 query 测冷路径 (每次真打 mlx)。两者都报。

#![cfg(feature = "mlx-live")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use fm_core::{FusionMemoryEngine, Interaction, RetrieveQuery, Turn};
use fm_embed::{EmbedConfig, Embedder, MlxEmbedder};
use fm_engine::MemoryEngine;
use fm_persist::Persist;
use fm_store::LocalStore;

// 规模受限 fusion-mlx rate limit bug (#692): _serve_from_model_dir 路径漏配
// configure_rate_limiter, 模块级 RateLimiter(60, enabled=True) 常驻 — 即使
// --rate-limit 0 也 60rpm。非本工程代码, 已提 issue #692 (上游 #635/#637 只修了另两路)。
// bench 控总 mlx embed 请求 < 60/min 避 429。故 seed/iter 极小, 数据为端到端 mlx
// 路径量级参考, 非大规模压测 (大规模压测用 retrieve_bench.rs StubEmbedder 免模型)。
// 证明: 真 bge-m3 HTTP 路径通 + 量级合理。#692 修后可放开规模重跑。
const TOTAL_MEMORIES: usize = 10;
const TURNS_PER_INTERACTION: u32 = 2;
const WARMUP: usize = 2;
const COLD_ITERS: usize = 5;
const CACHED_ITERS: usize = 20;
const CONCURRENCY: usize = 5;
const CONCURRENT_ITERS: usize = 2;
const DIM: usize = 1024;

fn live_cfg() -> EmbedConfig {
    let mut c = EmbedConfig::from_env();
    if c.api_key.is_empty() {
        c.api_key =
            std::env::var("FUSION_MEMORY_MLX_API_KEY").unwrap_or_else(|_| "change-me".into());
    }
    c.dimension = DIM;
    c
}

fn build_engine() -> MemoryEngine {
    let dir = std::env::temp_dir().join(format!("fm-bench-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(LocalStore::open(&dir, DIM).expect("store open"));
    let persist = Arc::new(Persist::open_in_memory().expect("persist open"));
    let cfg = live_cfg();
    let embedder: Arc<dyn Embedder> = Arc::new(MlxEmbedder::new(cfg).expect("mlx embedder"));
    MemoryEngine::new(store, persist, embedder)
}

async fn ping_mlx(engine: &MemoryEngine) {
    // 触发 bge-m3 首次加载 (冷启动), 避免污染后续计时。
    let q = RetrieveQuery::new("warmup ping", 1, 256);
    let t = Instant::now();
    let _ = engine.retrieve_context(&q).await;
    tracing::info!(
        secs = t.elapsed().as_secs_f64(),
        "mlx warmup ping done (bge-m3 loaded)"
    );
}

async fn seed(engine: &MemoryEngine) {
    let n_inter = TOTAL_MEMORIES / TURNS_PER_INTERACTION as usize;
    for i in 0..n_inter {
        let ix = Interaction {
            id: format!("ix-{i}"),
            session_id: format!("sess-{i}"),
            turns: (0..TURNS_PER_INTERACTION)
                .map(|t| Turn {
                    turn_idx: t,
                    user_message: format!("user ask {i} turn {t} rust memory topic"),
                    assistant_message: format!("assistant answer {i} turn {t}"),
                    tool_calls: vec![],
                })
                .collect(),
            timestamp: i as u64,
            metadata: serde_json::json!({}),
        };
        engine
            .commit_episodic_memory(&format!("sess-{i}"), &ix)
            .await
            .expect("commit seed");
    }
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn main() {
    println!("== §13.2 live perf baseline: bge-m3 dim={DIM} {TOTAL_MEMORIES} memories ==");
    let rt = tokio::runtime::Runtime::new().expect("tokio rt");
    let engine = rt.block_on(async {
        let e = build_engine();
        ping_mlx(&e).await;
        seed(&e).await;
        e
    });
    let engine = Arc::new(engine);

    // ---- 冷路径: 每次唯一 query, 真打 mlx (无缓存) ----
    let mut cold: Vec<Duration> = Vec::with_capacity(COLD_ITERS);
    rt.block_on(async {
        for _ in 0..WARMUP {
            let q = RetrieveQuery::new("warmup cold path query rust memory", 10, 512);
            let _ = engine.retrieve_context(&q).await;
        }
        for i in 0..COLD_ITERS {
            // 唯一 query 避缓存命中, 测真 mlx embed 延迟
            let q = RetrieveQuery::new(
                &format!("unique query {i} rust memory topic search"),
                10,
                512,
            );
            let t = Instant::now();
            let ctx = engine.retrieve_context(&q).await.expect("retrieve cold");
            cold.push(t.elapsed());
            assert!(
                !ctx.blocks.is_empty(),
                "cold retrieve must hit seeded memories"
            );
        }
    });
    cold.sort();
    let cold_p50 = percentile(&cold, 0.50);
    let cold_p99 = percentile(&cold, 0.99);
    println!(
        "cold  (unique query, no cache) retrieve: p50={:.3}ms p99={:.3}ms (real bge-m3 HTTP each)",
        cold_p50.as_secs_f64() * 1000.0,
        cold_p99.as_secs_f64() * 1000.0,
    );

    // ---- 热路径: 同 query, LRU 缓存命中 (跳 mlx HTTP) ----
    let mut cached: Vec<Duration> = Vec::with_capacity(CACHED_ITERS);
    let cached_query = RetrieveQuery::new("cached repeated query rust memory topic", 10, 512);
    rt.block_on(async {
        let _ = engine.retrieve_context(&cached_query).await; // 填缓存
        for _ in 0..CACHED_ITERS {
            let t = Instant::now();
            let ctx = engine
                .retrieve_context(&cached_query)
                .await
                .expect("retrieve cached");
            cached.push(t.elapsed());
            assert!(!ctx.blocks.is_empty());
        }
    });
    cached.sort();
    let cached_p50 = percentile(&cached, 0.50);
    let cached_p99 = percentile(&cached, 0.99);
    println!(
        "cached (LRU hit, skip mlx HTTP) retrieve: p50={:.3}ms p99={:.3}ms (index-layer only)",
        cached_p50.as_secs_f64() * 1000.0,
        cached_p99.as_secs_f64() * 1000.0,
    );

    // ---- 10 并发冷路径 ----
    let mut conc: Vec<Duration> = Vec::with_capacity(CONCURRENT_ITERS);
    rt.block_on(async {
        for _ in 0..2 {
            let mut h = Vec::new();
            for i in 0..CONCURRENCY {
                let e = Arc::clone(&engine);
                h.push(tokio::spawn(async move {
                    let q = RetrieveQuery::new(&format!("conc warmup {i} rust memory"), 10, 512);
                    let _ = e.retrieve_context(&q).await;
                }));
            }
            for h in h {
                let _ = h.await;
            }
        }
        for n in 0..CONCURRENT_ITERS {
            let t = Instant::now();
            let mut h = Vec::new();
            for i in 0..CONCURRENCY {
                let e = Arc::clone(&engine);
                // 唯一 query 避缓存, 测真并发 mlx 负载
                let q = RetrieveQuery::new(&format!("conc {n} q{i} rust memory topic"), 10, 512);
                h.push(tokio::spawn(async move { e.retrieve_context(&q).await }));
            }
            for h in h {
                let _ = h.await.expect("conc retrieve");
            }
            conc.push(t.elapsed());
        }
    });
    conc.sort();
    let conc_p50 = percentile(&conc, 0.50);
    let conc_p99 = percentile(&conc, 0.99);
    println!(
        "concurrent x{} (unique query, real mlx) retrieve: p50={:.3}ms p99={:.3}ms",
        CONCURRENCY,
        conc_p50.as_secs_f64() * 1000.0,
        conc_p99.as_secs_f64() * 1000.0,
    );

    // ---- 结果落 JSON ----
    let result = serde_json::json!({
        "_doc": "§13.2 live perf baseline (real fusion-mlx bge-m3 dim=1024). Closes RC known-limitation #1 (StubEmbedder perf).",
        "_embedder": "MlxEmbedder (bge-m3, dim=1024, HTTP /v1/embeddings)",
        "_machine": "Apple Silicon, release profile",
        "_run": "cargo bench -p fm-engine --features mlx-live --bench retrieve_bench_live (needs live fusion-mlx bge-m3)",
        "total_memories": TOTAL_MEMORIES,
        "dim": DIM,
        "cold_p50_ms": cold_p50.as_secs_f64() * 1000.0,
        "cold_p99_ms": cold_p99.as_secs_f64() * 1000.0,
        "cached_p50_ms": cached_p50.as_secs_f64() * 1000.0,
        "cached_p99_ms": cached_p99.as_secs_f64() * 1000.0,
        "concurrency": CONCURRENCY,
        "concurrent_p50_ms": conc_p50.as_secs_f64() * 1000.0,
        "concurrent_p99_ms": conc_p99.as_secs_f64() * 1000.0,
    });
    let out_path = std::env::temp_dir().join(format!("fm-perf-live-{}.json", std::process::id()));
    std::fs::write(&out_path, serde_json::to_string_pretty(&result).unwrap())
        .expect("write result");
    println!("result json: {}", out_path.display());

    // 清理 bench 过程数据, 只留 JSON + 日志 (全局规则: 验证完清过程数据)
    let dir = std::env::temp_dir().join(format!("fm-bench-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    println!("== live perf baseline done (bge-m3 real latency captured) ==");
}
