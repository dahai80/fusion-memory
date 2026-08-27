//! fm-store: 向量存储 trait + store-stub 长期生产后端。PRD §8。
//!
//! `FusionStoreEngine` trait（§8.1 补齐版）+ store-stub 实现（hnsw_rs + sled，
//! 持久化/崩溃恢复/tombstone/compact，PRD §8.2/§8.4）。
//!
//! §1.4: trait 唯一实作者 StoreStub 现经 `Arc<dyn FusionStoreEngine>` 动态分发 (MemoryEngine
//! 字段不再硬编码具体类型)。store-fusion 后端为 fusion.rs 占位 (上游就绪前 compile_error 阻断)。

pub mod error;
pub mod trait_def;

#[cfg(feature = "store-stub")]
pub mod stub;

// §1.4: store-fusion 后端占位模块 (非空壳: 显式 compile_error)。
#[cfg(feature = "store-fusion")]
pub mod fusion;

pub use error::{StoreError, StoreResult};
pub use trait_def::{FusionStoreEngine, ZeroCopyBuffer};

#[cfg(feature = "store-stub")]
pub use stub::StoreStub;
