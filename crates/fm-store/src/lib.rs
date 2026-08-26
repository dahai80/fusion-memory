//! fm-store: 向量存储 trait + store-stub 长期生产后端。PRD §8。
//!
//! `FusionStoreEngine` trait（§8.1 补齐版）+ store-stub 实现（hnsw_rs + sled，
//! 持久化/崩溃恢复/tombstone/compact，PRD §8.2/§8.4）。

pub mod error;
pub mod trait_def;

#[cfg(feature = "store-stub")]
pub mod stub;

pub use error::{StoreError, StoreResult};
pub use trait_def::{FusionStoreEngine, ZeroCopyBuffer};

#[cfg(feature = "store-stub")]
pub use stub::StoreStub;
