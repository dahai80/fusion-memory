---
doc_id: fusion-memory/architecture
product: fusion-memory
class: core
entry: service
doc_type: architecture
title: { zh: "fusion-memory 架构", en: "fusion-memory architecture" }
audience: engineer
status: draft
version: 1.5.0
updated: 2026-11-07
owner: "@dahai"
source_of_truth:
  - CLAUDE.md
  - README.md
  - Cargo.toml
---

# fusion-memory 架构（Architecture）

Rust workspace（edition 2021，MSRV 1.87），12 crates：

```text
fm-core/       数据结构（MemoryItem/EntityNode）与 FusionMemoryEngine trait
fm-engine/     记忆引擎：三级调度、遗忘曲线、巩固、PII 脱敏（redact.rs）、导入
fm-embed/      MlxEmbedder（bge-m3 dim=1024）+ StubEmbedder（FNV-1a 离线）
fm-similarity/ f32 余弦相似度（PRD 的 NEON SIMD 判定为过度设计，未采用）
fm-graph/      知识图谱：SQLite 递归 CTE 实现 N-hop（Kuzu 无 Rust binding，PRD 偏离记录见 README）
fm-store/      可选 store-fusion 适配器（默认 store-stub：hnsw_rs + sled，长期生产后端）
fm-persist/    持久化（schema v4，idx_memory_namespace）
fm-server/     UDS JSON-RPC（0600）+ HTTP axum（:11435）+ 内嵌 Web 控制台（include_dir 编译期嵌入）
fm-cli/        命令行入口
fm-cluster/    集群同步（leader-follower，全内网）
fm-fuzz/       模糊测试
fm-py/         PyO3 绑定（cdylib，测试套件默认排除）
```

## 数据流

```text
commit: interaction → 实体抽取(fusion-mlx chat model) → PII 脱敏 → 嵌入(bge-m3)
        → 向量入库(store-stub HNSW) + 图谱节点(SQLite CTE) + 持久化(schema v4)
retrieve: query → 嵌入 → 向量检索 + 图谱对齐 → fusion 评分(遗忘曲线衰减 + 新鲜度归一)
        → 组装压缩上下文 {context, memory_ids, cache_hit}
consolidate: 夜间任务 → 遗忘衰减 + 合并 → ConsolidationReport
```

## 关键决策（PRD 偏离均有记录）

- **SQLite 递归 CTE 替代 Kuzu**：Kuzu 无 Rust binding（README M2 偏离记录）。
- **store-stub 为长期生产后端**：fusion-store trait API 不匹配 + 跨仓约束（M5 偏离记录）；store-fusion 为可选 feature。
- **fusion 评分经消融校准**：原始 W(t) 融合劣于纯余弦，重调为 `freshness_of()` 归一化新鲜度后全量 fusion recall@5 反超 +0.40（docs/benchmarks/retrieval-quality-2026-10-25.md），CI 门禁锁行为。
- **fail-closed 边界**：PII 脱敏默认开、身份校验失败即拒、非法 namespace -32602、破坏性 RPC 必须显式 confirm。
- **100% 离线**：HTTP 仅 127.0.0.1，集群全内网，无云端调用。

## 生态位置

治理与上下文层，与 fusion-guard（TCC/DLP）、fusion-rag（AST）并列；上层为感知执行层（fusion-executor / fusion-browser / fusion-event），下层为基础设施层（fusion-core / fusion-store / fusion-mlx / fusion-gateway）。业务边界：向量页存储归 fusion-store，代码 AST 归 fusion-rag，本仓消费不重造。

## 一致性

构建/测试/lint 命令与门禁见 [get-started](get-started.md)；M0–M6 阶段记录与 PRD 偏离记录见 README。
