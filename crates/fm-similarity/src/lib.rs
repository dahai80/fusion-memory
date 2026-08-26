//! fm-similarity: 余弦相似度 + 遗忘曲线衰减 W(t)。PRD §7.1, §7.2。
//!
//! 衰减模型 B3 修正: W(t)=W0*exp(-t/τ(type))*min(1+log2(1+access_count), REINFORCE_CAP)。
//! t 基于 created_timestamp (解耦 last_accessed), 强化因子封顶防富者愈富。
//! 余弦相似度纯 Rust 实现 (NEON SIMD 由编译器自动矢化, 不引外部 crate)。

pub mod decay;
pub mod similarity;

pub use decay::{reinforce_factor, weight_at};
pub use similarity::cosine;
