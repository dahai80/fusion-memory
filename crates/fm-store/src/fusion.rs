//! §1.4: store-fusion 后端占位。PRD §8.1 的 fusion-store (HNSW 零拷贝 mmap) 接入点。
//!
//! 业务边界: 向量页存储归 `fusion-store` (上游), fusion-memory 消费其向量索引, 不重实现。
//! 当前 fusion-store trait API 对不上 + 跨工程约束 (见 README "M5 PRD 偏离记录"), 暂用 local-store
//! 做长期生产后端。此模块为 store-fusion feature 的编译态占位 — 命中时显式 compile_error,
//! 不再是"空壳 feature 产出无 store 实现的废 crate" (审计 §1.4 故障场景)。
//!
//! 上游就绪 (fusion-store#N trait 对齐) 后, 此文件替换为真实 FusionStoreEngine impl:
//!   - 接 fusion-store HNSW 索引 (零拷贝 mmap, 替 local-store 的 hnsw_rs+sled owned)
//!   - ZeroCopyBuffer.data 换 mmap 切片 (真零拷贝, 见 trait_def §3.16 注释)
//!   - 走 issue→PR 流程落地 (遵循 monorepo 跨工程约束)

#[cfg(not(feature = "local-store"))]
compile_error!(
    "store-fusion backend not yet implemented: fusion-store upstream trait alignment pending. \
     Build with default features (local-store) instead. See README \"M5 PRD 偏离记录\" + fusion-memory §1.4."
);
