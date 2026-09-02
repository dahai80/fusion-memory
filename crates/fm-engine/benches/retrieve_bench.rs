//! §13.2 perf 基线 bench: local-store 10k 条记忆下, 单条 retrieve p99<50ms,
//! 10 并发 retrieve p99<200ms。轻量手写 (无 criterion), 结果打印 + 落 JSON。
//!
//! 跑法: cargo bench -p fm-engine
//! 验收门 (PRD §13.2): single p99<50ms, concurrent p99<200ms。
//! perf gate 针对 local-store (非 mlx), 用 StubEmbedder 免模型。

use std::sync::Arc;
use std::time::{Duration, Instant};

use fm_core::{FusionMemoryEngine, Interaction, RetrieveQuery, Turn};
use fm_embed::StubEmbedder;
use fm_engine::MemoryEngine;
use fm_persist::Persist;
use fm_store::LocalStore;

const TOTAL_MEMORIES: usize = 10_000;
const TURNS_PER_INTERACTION: u32 = 2;
const SINGLE_TARGET_P99_MS: u64 = 50;
const CONCURRENT_TARGET_P99_MS: u64 = 200;
const CONCURRENCY: usize = 10;
const WARMUP: usize = 20;
const SINGLE_ITERS: usize = 200;
const CONCURRENT_ITERS: usize = 50;

fn build_engine() -> MemoryEngine {
    let dir = std::env::temp_dir().join(format!("fm-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(LocalStore::open(&dir, 64).expect("store open"));
    let persist = Arc::new(Persist::open_in_memory().expect("persist open"));
    let embedder: Arc<dyn fm_embed::Embedder> = Arc::new(StubEmbedder::new(64));
    MemoryEngine::new(store, persist, embedder)
}

async fn seed(engine: &MemoryEngine) {
    let n_inter = TOTAL_MEMORIES / TURNS_PER_INTERACTION as usize;
    for i in 0..n_inter {
        let ix = Interaction {
            id: format!("ix-{i}"),
            session_id: format!("sess-{i}"),
            tenant: String::new(),
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
    println!("== §13.2 perf baseline: local-store {TOTAL_MEMORIES} memories ==");
    let rt = tokio::runtime::Runtime::new().expect("tokio rt");
    let engine = rt.block_on(async {
        let engine = build_engine();
        seed(&engine).await;
        engine
    });
    let engine = Arc::new(engine);

    // ---- 单条 retrieve ----
    let mut single: Vec<Duration> = Vec::with_capacity(SINGLE_ITERS);
    let query = RetrieveQuery::new("user ask rust memory topic", 10, 4096);
    rt.block_on(async {
        // warmup
        for _ in 0..WARMUP {
            let _ = engine.retrieve_context(&query).await;
        }
        for _ in 0..SINGLE_ITERS {
            let t = Instant::now();
            let ctx = engine.retrieve_context(&query).await.expect("retrieve");
            let elapsed = t.elapsed();
            single.push(elapsed);
            assert!(!ctx.blocks.is_empty(), "retrieve must hit seeded memories");
        }
    });
    single.sort();
    let single_p99 = percentile(&single, 0.99);
    let single_p50 = percentile(&single, 0.50);
    println!(
        "single  retrieve: p50={:.3}ms p99={:.3}ms (target p99<{}ms) blocks per query verified non-empty",
        single_p50.as_secs_f64() * 1000.0,
        single_p99.as_secs_f64() * 1000.0,
        SINGLE_TARGET_P99_MS
    );

    // ---- 10 并发 retrieve ----
    let mut conc: Vec<Duration> = Vec::with_capacity(CONCURRENT_ITERS);
    rt.block_on(async {
        for _ in 0..WARMUP {
            let mut h = Vec::new();
            for _ in 0..CONCURRENCY {
                let e = Arc::clone(&engine);
                let q = query.clone();
                h.push(tokio::spawn(async move { e.retrieve_context(&q).await }));
            }
            for h in h {
                let _ = h.await;
            }
        }
        for _ in 0..CONCURRENT_ITERS {
            let t = Instant::now();
            let mut h = Vec::new();
            for _ in 0..CONCURRENCY {
                let e = Arc::clone(&engine);
                let q = query.clone();
                h.push(tokio::spawn(async move { e.retrieve_context(&q).await }));
            }
            for h in h {
                let _ = h.await.expect("conc retrieve");
            }
            conc.push(t.elapsed());
        }
    });
    conc.sort();
    let conc_p99 = percentile(&conc, 0.99);
    let conc_p50 = percentile(&conc, 0.50);
    println!(
        "concurrent x{} retrieve: p50={:.3}ms p99={:.3}ms (target p99<{}ms)",
        CONCURRENCY,
        conc_p50.as_secs_f64() * 1000.0,
        conc_p99.as_secs_f64() * 1000.0,
        CONCURRENT_TARGET_P99_MS
    );

    // ---- 结果落 JSON ----
    let result = serde_json::json!({
        "total_memories": TOTAL_MEMORIES,
        "single_p50_ms": single_p50.as_secs_f64() * 1000.0,
        "single_p99_ms": single_p99.as_secs_f64() * 1000.0,
        "single_target_p99_ms": SINGLE_TARGET_P99_MS,
        "single_pass": single_p99.as_millis() as u64 <= SINGLE_TARGET_P99_MS,
        "concurrent_p50_ms": conc_p50.as_secs_f64() * 1000.0,
        "concurrent_p99_ms": conc_p99.as_secs_f64() * 1000.0,
        "concurrency": CONCURRENCY,
        "concurrent_target_p99_ms": CONCURRENT_TARGET_P99_MS,
        "concurrent_pass": conc_p99.as_millis() as u64 <= CONCURRENT_TARGET_P99_MS,
    });
    let out_path =
        std::env::temp_dir().join(format!("fm-perf-baseline-{}.json", std::process::id()));
    std::fs::write(&out_path, serde_json::to_string_pretty(&result).unwrap())
        .expect("write result");
    println!("result json: {}", out_path.display());

    // ---- 验收门 ----
    let single_pass = single_p99.as_millis() as u64 <= SINGLE_TARGET_P99_MS;
    let conc_pass = conc_p99.as_millis() as u64 <= CONCURRENT_TARGET_P99_MS;
    if single_pass && conc_pass {
        println!("== PASS: §13.2 perf baseline met ==");
    } else {
        println!(
            "== FAIL: §13.2 perf baseline NOT met (single_pass={single_pass} conc_pass={conc_pass}) =="
        );
        std::process::exit(1);
    }
}
