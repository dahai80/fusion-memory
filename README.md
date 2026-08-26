# fusion-memory

Fusion 生态（"一核九端"）系统级长/短期记忆与认知图谱中枢。解决 Agent 跨 session 状态断层、重复提问、context window 爆炸，目标：越用越懂用户。

- 权威 PRD：`architecture/fusion-memory-prd-0825.md`
- 落地架构：`~/fusion/fusion-memory-prd-plan-0826.md`
- 审计报告：`~/fusion/audit/fusion-memory-audit-0826.md`

## 状态

**M2 已完成**：真实 bge-m3 embedding（dim=1024）+ 实体抽取（防注入 prompt + 严格 JSON 解析）+ SQLite 递归 CTE 图遍历 + 规则优先实体对齐 + 融合评分（cosine+衰减+graph_affinity）+ agent-studio 历史记忆导入。验收：实体抽取 JSON 解析成功率 100%（>90%），规则优先对齐正确（同名同 type 合并 / 同名异 type 不合并），真实 embedding 往返 dim=1024。测试覆盖率 lines 90.59% / regions 92.17%。162 测试全绿，clippy -D warnings 通过。

**M1 已完成**：store-stub 后端（hnsw_rs + sled）+ SQLite WAL 持久化 + StubEngine（确定性 stub embedding）+ CLI（commit/query/stats/delete/doctor）。验收：CLI 写 100 条（50 interaction × 2 turn）→ query 聚合每 block 还原 2 turn → doctor 报组件状态。测试覆盖率 lines 94.6% / regions 91.1%（cargo-llvm-cov）。

| 里程碑 | 内容 | 状态 |
|--------|------|------|
| M0 | workspace + 核心类型 + trait + CI | ✅ |
| M1 | store-stub 后端 + 引擎可跑（stub embedding）+ CLI | ✅ |
| M2 | 真实 embedding + 实体抽取 + 图 + 融合评分 + 导入 | ✅ |
| M3 | 服务化 + PyO3 + consolidate + 鉴权 | ⏳ |
| M4 | 消费方接入 | ⏳ |
| M5 | store-fusion 可选切换 + guard 旁路（可选） | ⏳ |
| M6 | 集群同步 leader-follower | ⏳ |

### M2 PRD 偏离记录（Rule 7）

- **Kuzu DB → SQLite 递归 CTE**（裁定 2026-08-26）：PRD §9.2 选 Kuzu DB 嵌入图，但 Kuzu 无 Rust binding。改用 SQLite 递归 CTE（`relation` 表 + `WITH RECURSIVE` N-hop 遍历），`fm-persist` 内实现，`fm-graph::graph_affinity` 消费。功能等价（N-hop 可达性 + 直接命中），无需额外 server 进程。

## 架构

- 核心 Rust（无 GC 停顿），SQLite WAL + SQLite 递归 CTE 图遍历（替代 Kuzu，见偏离记录），store-stub（hnsw_rs + sled，长期生产后端）
- 三级记忆：Working → Short-Term → Long-Term Graph
- 艾宾浩斯遗忘曲线 + 实体-关系认知图谱
- **turn 级存储**：单轮对话 = 一条 MemoryItem，检索按 `interaction_id` 聚合还原完整 Interaction
- 100% 离线（本机 + 内网集群），无云 API，HTTP 仅 127.0.0.1

## Crate 结构

| crate | 职责 |
|-------|------|
| `fm-core` | 核心数据结构 + `FusionMemoryEngine` trait（零业务依赖） |
| `fm-engine` | 引擎实现：MemoryEngine + 实体抽取 + 融合评分 + 衰减 + Long 晋升 |
| `fm-similarity` | 余弦相似度 + 衰减 W(t)（遗忘曲线 + 强化封顶） |
| `fm-graph` | 规则优先实体对齐（A5）+ alias 字典 + graph_affinity（N-hop） |
| `fm-store` | `FusionStoreEngine` trait + store-stub 后端 |
| `fm-embed` | fusion-mlx bge-m3 embedding（LRU+信号量）+ StubEmbedder |
| `fm-persist` | SQLite WAL 元数据 schema + CRUD + relation 表（递归 CTE 图遍历） |
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

覆盖率（需 llvm-tools，系统 llvm 或 rustup 组件）：

```bash
LLVM_COV=/opt/homebrew/opt/llvm/bin/llvm-cov \
LLVM_PROFDATA=/opt/homebrew/opt/llvm/bin/llvm-profdata \
cargo llvm-cov --workspace --summary-only
```

工具链：edition 2021，MSRV 1.87。系统 rustc（Homebrew）即可编译，无需 rustup。

## M1 数据流

```
commit  Interaction ──turn级拆分──> MemoryItem per turn
                                 ├── embed(text, dim)  [M1: FNV-1a 确定性 stub]
                                 ├── store.insert_vector(vec_id, vec)  [hnsw_rs + sled]
                                 └── persist.put_memory(item)  [SQLite WAL]
retrieve query ──embed──> store.search_knn(top_k)
         ──命中 interaction_id──> persist.list_by_interaction 补全全部 turns
         ──组装 ContextBlock──> token 预算截断 ──> FormattedContext
delete   persist.get_memory → store.delete_vector (tombstone) + persist.tombstone
```

- **vector_id** = FNV-1a(ulid_string) → u64，存于 `MemoryItem.vector_ref`
- **聚合还原**：检索命中 turn 后按 `interaction_id` 查 persist 全部 turn，`AGG_MAX_TURNS=20`
- **软删**：store tombstone + persist tombstone；`compact` 物理移除 + 重建 hnsw

## M2 数据流

```
commit  turn ──MlxEmbedder.embed──> store.insert_vector(dim=1024)
                 └── persist.put_memory(entities_pending=true) ──> 异步 extract_and_attach
                        └── MlxEntityExtractor(chat, 防注入 prompt) ──> 严格 JSON 解析 ──> entities 回写
retrieve query ──embed──> KNN ──> score_candidate = α·cosine + β·W(t) + γ·graph_affinity
                        α=0.5 β=0.3 γ=0.2; W(t)=W0·exp(-t/τ)·min(1+log2(1+count), CAP)
                        graph_affinity: 直接命中=1.0, N-hop=0.5^h (hop≤2), 否则 0
consolidate  W(t)<θ_drop(0.05) → tombstone 回收; Short→Long 晋升; entities_pending 批量重抽
import       agent-studio memories ──映射──> embed ──> 入库 (scope/metadata → Project 实体)
```

- **实体对齐规则优先链**（A5，hit-and-stop）：规则1 normalize+同名同 type(pri=3) → 规则2 alias 字典规范名(pri=2) → 规则3 existing 名/alias(pri=1) → 规则4 向量阈值(pri=0) → 新实体(pri=-1)。**同名异 type 不可合并**。
- **防注入**（§11.4）：对话内容用 `<data>` 标签包裹，prompt 明示忽略标签内指令；解析失败 → 空 entities，`entities_pending` 保持 true，content+vector 仍入库。
- **活体验收**：`FUSION_MEMORY_MLX_API_KEY=dahai168 cargo run -p fm-cli --example live_acceptance`（需 fusion-mlx 起 bge-m3 + Qwen3.5）。

## CLI 用法

```bash
cargo build --release -p fm-cli   # 产出 target/release/fm

# 写入一条多轮交互（JSON 从 stdin 或 --file）
echo '{"id":"ix-1","session_id":"s","turns":[{"turn_idx":0,"user_message":"hello rust","assistant_message":"hi","tool_calls":[]}],"timestamp":1,"metadata":{}}' \
  | ./target/release/fm --home ~/.fusion-memory --dim 64 commit --session s

./target/release/fm --home ~/.fusion-memory query --text "hello rust" --top-k 5 --budget 4096
./target/release/fm --home ~/.fusion-memory stats
./target/release/fm --home ~/.fusion-memory delete --id <memory_id> --confirm
./target/release/fm --home ~/.fusion-memory doctor

# 从 fusion-agent-studio 历史记忆导入 (真 bge-m3, dim=1024)
FUSION_MEMORY_MLX_API_KEY=dahai168 ./target/release/fm --home ~/.fusion-memory import
# 离线测试导入 (--stub 用 StubEmbedder, dim=64)
./target/release/fm --home ~/.fusion-memory import --stub --source /path/to/memory.db
```

`--home` 默认 `~/.fusion-memory`（或 `FM_HOME` 环境变量），`--dim` 默认 64（stub）；真实 embedding 走 bge-m3 dim=1024，`import` 不用 `--stub` 时自动用 1024。
`import` 映射：tier short_term→Short / long_term→Long / archive→跳过；memory_type user→Semantic / feedback→Procedural / project→Episodic / reference→Semantic；scope `graph:NAME` 或 metadata.graph_id → Project 实体。

## 约定

- 4 空格缩进，无 docstring（`//!` 模块文档 + 行内注释）
- `tracing` 日志，`anyhow`（应用）+ `thiserror`（库）
- 失败可见，不静默吞错
