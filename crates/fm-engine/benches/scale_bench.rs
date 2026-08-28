//! P1-7: 规模验证 bench。local-store 10k/100k/1M 向量下, 测:
//! - seed (insert) 总时 + 吞吐
//! - rebuild_from_sled (重启重建 HNSW) 时
//! - 单条 search_knn p99
//! - 10 并发 retrieve_context p99 (全引擎路径, 含 scoring+persist 元数据)
//! - sled 数据目录磁盘占用
//!
//! 跑法 (规模经 env FM_SCALE 选):
//!   cargo bench -p fm-engine --bench scale_bench                  # 默认 100k
//!   FM_SCALE=10k  cargo bench -p fm-engine --bench scale_bench
//!   FM_SCALE=100k cargo bench -p fm-engine --bench scale_bench
//!   FM_SCALE=1m   cargo bench -p fm-engine --bench scale_bench    # 慢, 一次性验证用
//!   FM_CONCURRENCY=1,10,50,100,200 cargo bench -p fm-engine --bench scale_bench  # v1.0.0 并发梯度
//!
//! seed/rebuild/knn 直接打 local-store (FusionStoreEngine trait), 聚焦向量规模本身
//! (P1-7 audit 点: scale unverified, 仅 10k)。并发测走全引擎 retrieve_context 贴真实负载。
//! StubEmbedder 免模型, 确定性向量。

use std::sync::Arc;
use std::time::{Duration, Instant};

use fm_core::{FusionMemoryEngine, RetrieveQuery};
use fm_embed::StubEmbedder;
use fm_engine::MemoryEngine;
use fm_persist::Persist;
use fm_store::{FusionStoreEngine, LocalStore};

const WARMUP: usize = 20;
const SINGLE_ITERS: usize = 200;
const CONCURRENT_ITERS: usize = 50;

fn parse_scale() -> usize {
    match std::env::var("FM_SCALE").as_deref() {
        Ok("10k") => 10_000,
        Ok("100k") => 100_000,
        Ok("1m") => 1_000_000,
        _ => 100_000,
    }
}

fn dir_size_bytes(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    total += meta.len();
                } else if meta.is_dir() {
                    total += dir_size_bytes(&entry.path());
                }
            }
        }
    }
    total
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn main() {
    let total = parse_scale();
    println!("== P1-7 scale bench: local-store {total} vectors ==");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio rt");

    let dir = tempfile::TempDir::new().expect("temp dir");
    // 具体类型 Arc<LocalStore>: 可调 inherent flush/rebuild_from_sled (非 trait 方法)。
    // store_dyn: trait object, 供 MemoryEngine + trait 路径 knn 测。
    let store: Arc<LocalStore> = Arc::new(LocalStore::open(dir.path(), 64).expect("store open"));
    let store_dyn: Arc<dyn FusionStoreEngine> = store.clone();
    let persist = Arc::new(Persist::open_in_memory().expect("persist open"));
    let embedder: Arc<dyn fm_embed::Embedder> = Arc::new(StubEmbedder::new(64));
    let engine = Arc::new(MemoryEngine::new(store_dyn.clone(), persist, embedder));

    // ---- seed: 逐条 insert_vector, 测总时 + 吞吐 ----
    let seed_start = Instant::now();
    rt.block_on(async {
        for i in 1..=total {
            let mut v = vec![0.1f32; 64];
            v[0] = (i as f32) / (total as f32);
            store_dyn.insert_vector(i as u64, &v).expect("insert seed");
        }
        let _ = store.flush();
    });
    let seed_secs = seed_start.elapsed().as_secs_f64();
    let seed_throughput = total as f64 / seed_secs;
    println!(
        "seed    {total} vectors: {:.2}s ({:.0} vec/s)",
        seed_secs, seed_throughput
    );

    // ---- rebuild_from_sled: 模拟重启重建 HNSW 索引 (inherent, 直接打具体 LocalStore) ----
    let rebuild_start = Instant::now();
    let loaded = rt.block_on(async { store.rebuild_from_sled().expect("rebuild") });
    let rebuild_secs = rebuild_start.elapsed().as_secs_f64();
    println!(
        "rebuild HNSW from sled: {:.2}s (loaded {loaded} vectors)",
        rebuild_secs
    );

    // ---- 磁盘占用 ----
    let disk_mb = dir_size_bytes(dir.path()) as f64 / (1024.0 * 1024.0);
    println!("sled disk: {:.1} MB", disk_mb);

    // ---- 单条 search_knn (直接打 store trait object) ----
    let query = vec![0.1f32; 64];
    let mut single: Vec<Duration> = Vec::with_capacity(SINGLE_ITERS);
    rt.block_on(async {
        for _ in 0..WARMUP {
            let _ = store_dyn.search_knn(&query, 10).expect("warmup knn");
        }
        for _ in 0..SINGLE_ITERS {
            let t = Instant::now();
            let res = store_dyn.search_knn(&query, 10).expect("knn");
            single.push(t.elapsed());
            assert!(!res.is_empty(), "knn must hit seeded vectors");
        }
    });
    single.sort();
    let single_p99 = percentile(&single, 0.99);
    let single_p50 = percentile(&single, 0.50);
    println!(
        "single  knn: p50={:.3}ms p99={:.3}ms (n={})",
        single_p50.as_secs_f64() * 1000.0,
        single_p99.as_secs_f64() * 1000.0,
        SINGLE_ITERS
    );

    // ---- 并发梯度 retrieve_context (全引擎路径) ----
    // 商用级负载验证: 默认梯度 1/10/50/100, env FM_CONCURRENCY 覆盖 (逗号分隔)。
    let conc_levels: Vec<usize> = std::env::var("FM_CONCURRENCY")
        .map(|s| s.split(',').filter_map(|n| n.trim().parse().ok()).collect())
        .unwrap_or_else(|_| vec![1, 10, 50, 100]);
    let rq = RetrieveQuery::new("user ask rust memory topic", 10, 4096);
    let mut conc_results: Vec<(usize, f64, f64)> = Vec::new();
    for &c in &conc_levels {
        let mut conc: Vec<Duration> = Vec::with_capacity(CONCURRENT_ITERS);
        rt.block_on(async {
            for _ in 0..WARMUP.min(CONCURRENT_ITERS) {
                let mut h = Vec::new();
                for _ in 0..c {
                    let e = Arc::clone(&engine);
                    let q = rq.clone();
                    h.push(tokio::spawn(async move { e.retrieve_context(&q).await }));
                }
                for h in h {
                    let _ = h.await;
                }
            }
            for _ in 0..CONCURRENT_ITERS {
                let t = Instant::now();
                let mut h = Vec::new();
                for _ in 0..c {
                    let e = Arc::clone(&engine);
                    let q = rq.clone();
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
            "concurrent x{} retrieve: p50={:.3}ms p99={:.3}ms (n={})",
            c,
            conc_p50.as_secs_f64() * 1000.0,
            conc_p99.as_secs_f64() * 1000.0,
            CONCURRENT_ITERS
        );
        conc_results.push((
            c,
            conc_p50.as_secs_f64() * 1000.0,
            conc_p99.as_secs_f64() * 1000.0,
        ));
    }

    // ---- 结果落 JSON ----
    let conc_json: Vec<serde_json::Value> = conc_results
        .iter()
        .map(|(c, p50, p99)| serde_json::json!({"concurrency": c, "p50_ms": p50, "p99_ms": p99}))
        .collect();
    let result = serde_json::json!({
        "_doc": "P1-7 scale bench (local-store, StubEmbedder dim=64). audit §8 P1-7. v1.0.0 加并发梯度 (商用负载)。",
        "_run": "FM_SCALE=<10k|100k|1m> [FM_CONCURRENCY=1,10,50,100] cargo bench -p fm-engine --bench scale_bench",
        "_machine": "Apple Silicon, release profile",
        "total_vectors": total,
        "seed_secs": seed_secs,
        "seed_throughput_vec_per_s": seed_throughput,
        "rebuild_secs": rebuild_secs,
        "rebuild_loaded": loaded,
        "sled_disk_mb": disk_mb,
        "single_p50_ms": single_p50.as_secs_f64() * 1000.0,
        "single_p99_ms": single_p99.as_secs_f64() * 1000.0,
        "concurrency_sweep": conc_json,
    });
    let out_path = std::env::temp_dir().join(format!(
        "fm-scale-bench-{}-{}.json",
        total,
        std::process::id()
    ));
    std::fs::write(&out_path, serde_json::to_string_pretty(&result).unwrap())
        .expect("write result");
    println!("result json: {}", out_path.display());
}
