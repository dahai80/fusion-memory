# fusion-memory

Fusion 生态（"一核九端"）系统级长/短期记忆与认知图谱中枢。解决 Agent 跨 session 状态断层、重复提问、context window 爆炸，目标：越用越懂用户。

- 权威 PRD：`architecture/fusion-memory-prd-0825.md`
- 落地架构：`~/fusion/fusion-memory-prd-plan-0826.md`
- 审计报告：`~/fusion/audit/fusion-memory-audit-0826.md`

## 状态

**M0 已完成**：Cargo workspace + 9 crate 骨架 + `fm-core` 全类型 + `FusionMemoryEngine` trait + CI。

| 里程碑 | 内容 | 状态 |
|--------|------|------|
| M0 | workspace + 核心类型 + trait + CI | ✅ |
| M1 | store-stub 后端 + 引擎可跑（stub embedding） | ⏳ |
| M2 | 真实 embedding + 实体抽取 + 图 | ⏳ |
| M3 | 服务化 + PyO3 + consolidate + 鉴权 | ⏳ |
| M4 | 消费方接入 | ⏳ |
| M5 | store-fusion 可选切换 + guard 旁路（可选） | ⏳ |
| M6 | 集群同步 leader-follower | ⏳ |

## 架构

- 核心 Rust（无 GC 停顿），SQLite WAL + Kuzu DB（嵌入图），store-stub（hnsw_rs + sled，长期生产后端）
- 三级记忆：Working → Short-Term → Long-Term Graph
- 艾宾浩斯遗忘曲线 + 实体-关系认知图谱
- **turn 级存储**：单轮对话 = 一条 MemoryItem，检索按 `interaction_id` 聚合还原完整 Interaction
- 100% 离线（本机 + 内网集群），无云 API

## Crate 结构

| crate | 职责 |
|-------|------|
| `fm-core` | 核心数据结构 + `FusionMemoryEngine` trait（零业务依赖） |
| `fm-engine` | 引擎实现：三级调度 + 召回评分 + 集群同步 |
| `fm-similarity` | NEON SIMD 余弦相似度 + 衰减计算 |
| `fm-graph` | Kuzu 嵌入图：schema + 图谱对齐 + graph_affinity |
| `fm-store` | `FusionStoreEngine` trait + store-stub 后端 |
| `fm-embed` | fusion-mlx embedding/chat HTTP 桥（唯一发 HTTP） |
| `fm-persist` | SQLite 元数据 schema + CRUD |
| `fm-server` | UDS JSON-RPC + HTTP 服务 |
| `fm-py` | PyO3 Python 绑定 |
| `fm-cli` | CLI 运维/导入/查询 |

## 构建

```bash
cargo check --workspace        # 编译检查
cargo test --workspace         # 全测试
cargo clippy --workspace --all-targets -- -D warnings   # lint
cargo fmt --all --check        # 格式检查
```

工具链：edition 2021，MSRV 1.87。系统 rustc（Homebrew）即可编译，无需 rustup。

## 约定

- 4 空格缩进，无 docstring（`//!` 模块文档 + 行内注释）
- `tracing` 日志，`anyhow`（应用）+ `thiserror`（库）
- 失败可见，不静默吞错
