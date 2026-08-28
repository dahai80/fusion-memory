//! P0-2: Prometheus 文本格式 metrics。无外部依赖 (Rule 2), 手写 AtomicU64 计数器。
//!
//! 暴露:
//! - http_requests_total (counter)
//! - http_errors_total (counter)
//! - http_request_duration_seconds (histogram, per-method label)
//! - engine_embedder_in_flight (gauge)
//! - engine_consolidate_running (gauge)
//! - store_pool_in_use (gauge)
//!
//! histogram 用固定 bucket (exponential), 每桶计数器, sum + count。无锁, AtomicU64/AtomicUsize。

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

/// 固定延迟 bucket (秒)。覆盖 0.5ms ~ 30s。
const LATENCY_BUCKETS: [f64; 10] = [0.0005, 0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 30.0];

/// HTTP 请求计数/延迟/错误 指标。
pub struct HttpMetrics {
    requests_total: AtomicU64,
    errors_total: AtomicU64,
    /// 每桶计数: LATENCY_BUCKETS.len() + 1 (+Inf 桶)。
    latency_bucket_counts: Vec<AtomicU64>,
    latency_sum: std::sync::atomic::AtomicU64, // 累加纳秒, 避浮点原子
    latency_count: AtomicU64,
    /// embedder 在飞并发 (gauge)。
    embedder_in_flight: AtomicUsize,
    /// consolidate 是否运行中 (0/1 gauge)。
    consolidate_running: AtomicUsize,
    /// r2d2 连接池在用连接数 (gauge)。
    pool_in_use: AtomicUsize,
}

impl HttpMetrics {
    pub fn new() -> Arc<Self> {
        let mut buckets = Vec::with_capacity(LATENCY_BUCKETS.len() + 1);
        for _ in 0..(LATENCY_BUCKETS.len() + 1) {
            buckets.push(AtomicU64::new(0));
        }
        Arc::new(Self {
            requests_total: AtomicU64::new(0),
            errors_total: AtomicU64::new(0),
            latency_bucket_counts: buckets,
            latency_sum: std::sync::atomic::AtomicU64::new(0),
            latency_count: AtomicU64::new(0),
            embedder_in_flight: AtomicUsize::new(0),
            consolidate_running: AtomicUsize::new(0),
            pool_in_use: AtomicUsize::new(0),
        })
    }

    pub fn incr_total(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn incr_error(&self) {
        self.errors_total.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录请求延迟 (秒)。落桶 + 累加 sum(纳秒) + count++。
    pub fn observe_duration(&self, _method: &str, secs: f64) {
        let nanos = (secs * 1e9) as u64;
        self.latency_sum.fetch_add(nanos, Ordering::Relaxed);
        self.latency_count.fetch_add(1, Ordering::Relaxed);
        // 找第一个 >= secs 的桶; 全超 → +Inf 桶。
        let mut idx = LATENCY_BUCKETS.len();
        for (i, b) in LATENCY_BUCKETS.iter().enumerate() {
            if secs <= *b {
                idx = i;
                break;
            }
        }
        self.latency_bucket_counts[idx].fetch_add(1, Ordering::Relaxed);
    }

    /// embedder 并发 gauge。
    pub fn embedder_inc(&self) {
        self.embedder_in_flight.fetch_add(1, Ordering::Relaxed);
    }
    pub fn embedder_dec(&self) {
        self.embedder_in_flight.fetch_sub(1, Ordering::Relaxed);
    }
    /// consolidate 运行状态 gauge。
    pub fn consolidate_set(&self, running: bool) {
        self.consolidate_running
            .store(if running { 1 } else { 0 }, Ordering::Relaxed);
    }
    /// 连接池在用数 gauge。
    pub fn pool_set(&self, n: usize) {
        self.pool_in_use.store(n, Ordering::Relaxed);
    }

    /// 渲染 Prometheus 文本格式。
    pub fn render_prometheus(&self) -> String {
        let mut out = String::with_capacity(2048);
        out.push_str("# HELP http_requests_total Total HTTP requests.\n");
        out.push_str("# TYPE http_requests_total counter\n");
        out.push_str(&format!(
            "http_requests_total {}\n",
            self.requests_total.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP http_errors_total Total HTTP error responses.\n");
        out.push_str("# TYPE http_errors_total counter\n");
        out.push_str(&format!(
            "http_errors_total {}\n",
            self.errors_total.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP http_request_duration_seconds Request latency histogram.\n");
        out.push_str("# TYPE http_request_duration_seconds histogram\n");
        let count = self.latency_count.load(Ordering::Relaxed);
        let sum_secs = self.latency_sum.load(Ordering::Relaxed) as f64 / 1e9;
        let mut cumulative: u64 = 0;
        for (i, b) in LATENCY_BUCKETS.iter().enumerate() {
            cumulative += self.latency_bucket_counts[i].load(Ordering::Relaxed);
            out.push_str(&format!(
                "http_request_duration_seconds_bucket{{le=\"{b}\"}} {cumulative}\n"
            ));
        }
        cumulative += self.latency_bucket_counts[LATENCY_BUCKETS.len()].load(Ordering::Relaxed);
        out.push_str(&format!(
            "http_request_duration_seconds_bucket{{le=\"+Inf\"}} {cumulative}\n"
        ));
        out.push_str(&format!("http_request_duration_seconds_sum {sum_secs}\n"));
        out.push_str(&format!("http_request_duration_seconds_count {count}\n"));
        out.push_str("# HELP engine_embedder_in_flight Embedder concurrent calls in flight.\n");
        out.push_str("# TYPE engine_embedder_in_flight gauge\n");
        out.push_str(&format!(
            "engine_embedder_in_flight {}\n",
            self.embedder_in_flight.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP engine_consolidate_running Consolidate saga running (0/1).\n");
        out.push_str("# TYPE engine_consolidate_running gauge\n");
        out.push_str(&format!(
            "engine_consolidate_running {}\n",
            self.consolidate_running.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP store_pool_in_use r2d2 pool connections in use.\n");
        out.push_str("# TYPE store_pool_in_use gauge\n");
        out.push_str(&format!(
            "store_pool_in_use {}\n",
            self.pool_in_use.load(Ordering::Relaxed)
        ));
        out
    }
}

impl Default for HttpMetrics {
    fn default() -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            errors_total: AtomicU64::new(0),
            latency_bucket_counts: (0..LATENCY_BUCKETS.len() + 1)
                .map(|_| AtomicU64::new(0))
                .collect(),
            latency_sum: std::sync::atomic::AtomicU64::new(0),
            latency_count: AtomicU64::new(0),
            embedder_in_flight: AtomicUsize::new(0),
            consolidate_running: AtomicUsize::new(0),
            pool_in_use: AtomicUsize::new(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_contains_all_metrics() {
        let m = HttpMetrics::new();
        m.incr_total();
        m.incr_total();
        m.incr_error();
        m.observe_duration("commit", 0.002);
        m.observe_duration("retrieve", 2.0);
        m.embedder_inc();
        m.consolidate_set(true);
        m.pool_set(3);
        let s = m.render_prometheus();
        assert!(s.contains("http_requests_total 2"), "s={s}");
        assert!(s.contains("http_errors_total 1"), "s={s}");
        assert!(s.contains("http_request_duration_seconds_count 2"), "s={s}");
        assert!(s.contains("le=\"0.005\""), "s={s}");
        assert!(s.contains("le=\"+Inf\""), "s={s}");
        assert!(s.contains("engine_embedder_in_flight 1"), "s={s}");
        assert!(s.contains("engine_consolidate_running 1"), "s={s}");
        assert!(s.contains("store_pool_in_use 3"), "s={s}");
    }

    #[test]
    fn latency_buckets_cumulative() {
        let m = HttpMetrics::new();
        // 0.002 落 0.005 桶 (idx 2), 0.0001 落 0.0005 桶 (idx 0)
        m.observe_duration("a", 0.002);
        m.observe_duration("a", 0.0001);
        let s = m.render_prometheus();
        // le=0.0005 桶应只含 1 个 (0.0001), le=0.005 含 2 个 (累计)
        assert!(s.contains("le=\"0.0005\"} 1"), "s={s}");
        assert!(s.contains("le=\"0.005\"} 2"), "s={s}");
        assert!(s.contains("le=\"+Inf\"} 2"), "s={s}");
    }

    #[test]
    fn embedder_gauge_inc_dec() {
        let m = HttpMetrics::new();
        m.embedder_inc();
        m.embedder_inc();
        m.embedder_dec();
        let s = m.render_prometheus();
        assert!(s.contains("engine_embedder_in_flight 1"), "s={s}");
    }
}
