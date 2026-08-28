//! fm-store: 向量存储 trait + local-store 长期生产后端。PRD §8。
//!
//! `FusionStoreEngine` trait（§8.1 补齐版）+ local-store 实现（hnsw_rs + sled，
//! 持久化/崩溃恢复/tombstone/compact，PRD §8.2/§8.4）。
//!
//! §1.4: trait 唯一实作者 LocalStore 现经 `Arc<dyn FusionStoreEngine>` 动态分发 (MemoryEngine
//! 字段不再硬编码具体类型)。store-fusion 后端为 fusion.rs 占位 (上游就绪前 compile_error 阻断)。

pub mod error;
pub mod trait_def;

#[cfg(feature = "local-store")]
pub mod local;

// §1.4: store-fusion 后端 (上游 fusion-store#3 trait 对齐已落地, fs-core rev 47d5b83)。
#[cfg(feature = "store-fusion")]
pub mod fusion;

pub use error::{StoreError, StoreResult};
pub use trait_def::{FusionStoreEngine, ZeroCopyBuffer};

#[cfg(feature = "local-store")]
pub use local::LocalStore;

#[cfg(feature = "store-fusion")]
pub use fusion::FusionStore;
