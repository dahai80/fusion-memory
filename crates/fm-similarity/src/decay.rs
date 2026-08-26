//! 遗忘曲线衰减 W(t)。PRD §7.1 (B3 修正: 封顶 + 衰减与强化解耦)。
//!
//! W(t) = W0 * exp(-t/τ(type)) * min(1+log2(1+access_count), REINFORCE_CAP)
//!   t = now - created_timestamp (秒), 不基于 last_accessed
//!   τ 来自 MemoryType::tau_seconds
//!   REINFORCE_CAP = 5.0 (封顶防富者愈富)
//!   access_count 强化因子与衰减解耦 (召回不重置 last_accessed)

use fm_core::MemoryType;

/// 强化封顶 (PRD §7.1 B3)。
pub const REINFORCE_CAP: f64 = 5.0;

/// 强化因子 min(1+log2(1+access_count), REINFORCE_CAP)。
/// 封顶防高频召回记忆强化因子无界膨胀 (B3 富者愈富)。
pub fn reinforce_factor(access_count: u64) -> f64 {
    let raw = 1.0 + (access_count as f64 + 1.0).log2();
    raw.min(REINFORCE_CAP)
}

/// 衰减权重 W(t)。PRD §7.1。
///
/// - `w0`: 初始权重 (MemoryType::initial_weight)
/// - `created_timestamp`: 创建时间 (秒或毫秒, 与 now 同单位即可)
/// - `now`: 当前时间 (同 created 单位)
/// - `mem_type`: 决定 τ
/// - `access_count`: 累计召回次数
pub fn weight_at(
    w0: f64,
    created_timestamp: u64,
    now: u64,
    mem_type: MemoryType,
    access_count: u64,
) -> f64 {
    let dt = now.saturating_sub(created_timestamp) as f64;
    let tau = mem_type.tau_seconds();
    let decay = (-dt / tau).exp();
    w0 * decay * reinforce_factor(access_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reinforce_zero_access() {
        // access_count=0 → 1+log2(1+0)=1+0=1.0
        assert!((reinforce_factor(0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn reinforce_monotonic_until_cap() {
        let mut prev = 0.0f64;
        for n in 0u64..100 {
            let f = reinforce_factor(n);
            assert!(f >= prev, "not monotonic at n={n} f={f}");
            prev = f;
        }
    }

    #[test]
    fn reinforce_capped_at_5() {
        // 高频召回应封顶 5.0, 不无限增长
        assert!((reinforce_factor(1_000_000) - REINFORCE_CAP).abs() < 1e-9);
        assert!((reinforce_factor(u64::MAX / 2) - REINFORCE_CAP).abs() < 1e-9);
    }

    #[test]
    fn weight_decays_over_time() {
        // Episodic τ=1天=86400秒。1 天后 W≈W0*exp(-1)*1
        let w0 = MemoryType::Episodic.initial_weight();
        let w1 = weight_at(w0, 0, 86_400, MemoryType::Episodic, 0);
        let expected = w0 * (-1.0f64).exp();
        assert!((w1 - expected).abs() < 1e-6, "w1={w1} expected={expected}");
        assert!(w1 < w0, "should decay");
    }

    #[test]
    fn weight_uses_created_not_last_accessed() {
        // B3 解耦: t 基于 created, 与 last_accessed 无关
        // 同 created 不同 now → 不同 W; created 远 now 近也衰减
        let w0 = MemoryType::Semantic.initial_weight();
        let w_recent = weight_at(w0, 0, 100, MemoryType::Semantic, 0);
        let w_old = weight_at(w0, 0, 1_000_000, MemoryType::Semantic, 0);
        assert!(w_recent > w_old, "older should decay more");
    }

    #[test]
    fn weight_high_access_not_dominating() {
        // B3 富者愈富防护: 高频召回记忆强化封顶, 不永久霸占
        // 极高频 access (封顶) vs 中频, 差距不超过 5x
        let w0 = 0.6;
        let w_low = weight_at(w0, 0, 86_400, MemoryType::Episodic, 1);
        let w_high = weight_at(w0, 0, 86_400, MemoryType::Episodic, 100_000);
        assert!(w_high / w_low <= REINFORCE_CAP + 1e-6, "cap violated");
    }

    #[test]
    fn weight_tau_by_type() {
        // Procedural τ=90天 >> Episodic τ=1天, 同时间 Procedural 衰减慢
        let w0 = 1.0;
        let t = 86_400 * 7; // 7 天
        let wepi = weight_at(w0, 0, t, MemoryType::Episodic, 0);
        let wproc = weight_at(w0, 0, t, MemoryType::Procedural, 0);
        assert!(wproc > wepi, "procedural should decay slower");
    }

    #[test]
    fn weight_now_before_created_clamps() {
        // now < created 时 saturating_sub → 0, decay=1, 不产生负数/NaN
        let w = weight_at(0.6, 1000, 500, MemoryType::Episodic, 0);
        assert!(w.is_finite());
        assert!((w - 0.6).abs() < 1e-6, "w={w}");
    }

    #[test]
    fn weight_at_creation_is_w0() {
        // t=0 时 decay=1, reinforce(0)=1 → W=W0
        let w0 = MemoryType::Semantic.initial_weight();
        let w = weight_at(w0, 100, 100, MemoryType::Semantic, 0);
        assert!((w - w0).abs() < 1e-9, "w={w}");
    }
}
