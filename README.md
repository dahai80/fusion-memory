# fusion-memory

Fusion 生态（"一核九端"）系统级长/短期记忆与认知图谱中枢。解决 Agent 跨 session 状态断层、重复提问、context window 爆炸，目标：越用越懂用户。

- 权威 PRD：`architecture/fusion-memory-prd-0825.md`
- 落地架构：`~/fusion/fusion-memory-prd-plan-0826.md`
- 审计报告：`~/fusion/audit/fusion-memory-audit-0826.md`

## 状态

**M2 已完成**：真实 bge-m3 embedding（dim=1024）+ 实体抽取（防注入 prompt + 严格 JSON 解析）+ SQLite 递归 CTE 图遍历 + 规则优先实体对齐 + 融合评分（cosine+衰减+graph_affinity）+ agent-studio 历史记忆导入。验收：实体抽取 JSON 解析成功率 100%（>90%），规则优先对齐正确（同名同 type 合并 / 同名异 type 不合并），真实 embedding 往返 dim=1024。测试覆盖率 lines 90.59% / regions 92.17%。162 测试全绿，clippy -D warnings 通过。

**M3 已完成**：fm-server（UDS JSON-RPC 0600 + HTTP axum 强制 Bearer B5，端口 11435，无 API_KEY 拒启 HTTP）+ fm-py PyO3 绑定（`allow_threads` GIL 安全 C2）+ consolidate_memories saga（增量遗忘 + merge/summarize/reconcile，跨库对账 + merge_log + unmerge）+ fm-cli（consolidate/merges/unmerge/reconcile）+ start.sh（start/stop/restart/status/log/doctor）。验收：PyO3 往返 GIL 不冻结（commit→2 ids / retrieve→block / consolidate→report）；HTTP 无 token 被拒 + DELETE 无 confirm 被拒 + 无 API_KEY 拒启 HTTP；consolidate 报告字段完整 + 对账差异检出；start.sh 三命令可用。242 离线 + live 测试全绿，regions 离线 90.63% / live 92.07%。

**M4 in-scope 已完成**（消费方接入参考实现 + 契约测试，本仓库内）：`clients/` 三消费方参考客户端 — TS HTTP 客户端（`ts/fusionMemoryClient.ts`，fusion-code vendor，默认端口 11440 避让 fusion-kb 11435）+ Python HTTP 客户端（`python/fusion_memory_client.py`，cowork/agent-studio 备选路径，默认 11435）+ `clients/README.md` 接入文档（协议矩阵 + wire 契约 + 三消费方接入缝 + port 冲突告警 + agent-studio 9 handler→6 RPC 映射表）。契约场景测试 `crates/fm-server/tests/consumer_scenarios.rs`（3 场景：cowork memory_commit/retrieve 节点流、fusion-code retrieve 注入→commit→跨 turn 召回、agent-studio 9 handler 后端替换映射 + delete 无 confirm -32602）。stub engine HTTP oneshot 往返，离线无 mlx。验收：3 契约场景 pass + 248 离线测试全绿 + clippy/fmt clean + regions 91.76%（升，新场景扩 trait path 覆盖）。**outward PR 已落地**（跨工程，3 消费方仓库 issue→PR→land）：fusion-cowork #67→#68（merged，memory_commit/retrieve 两节点）、fusion-agent-studio #246→#247（merged，FusionMemoryAdapter 9 handler→6 RPC env-gated swap）、fusion-code #150→#151（merged 5311b00，turn-end commit；retrieve-inject 半延后，tracked #154）。三消费方接入文件均已在各仓库 main 验证存在。

**M1 已完成**：store-stub 后端（hnsw_rs + sled）+ SQLite WAL 持久化 + StubEngine（确定性 stub embedding）+ CLI（commit/query/stats/delete/doctor）。验收：CLI 写 100 条（50 interaction × 2 turn）→ query 聚合每 block 还原 2 turn → doctor 报组件状态。测试覆盖率 lines 94.6% / regions 91.1%（cargo-llvm-cov）。

**M6 已完成**：集群同步 leader-follower（PRD §16 内网离线集群，非公网云）。新 crate `fm-cluster`：角色注入（standalone/leader/follower，env `FUSION_MEMORY_ROLE` > home/role 文件 > standalone）+ wop_log 复制（leader 单写点 + append_wop，follower 拉 SyncRequest → 本地重放 commit/delete，summarize 审计跳过）+ TCP 传输（4B 长度前缀 + JSON 线帧，Hello/SyncRequest/SyncResponse/Ping/Pong，内网端口 11436）+ 心跳（5s ping，连续 3 失败 = LeaderDown）+ 手动 failover（`fm cluster promote` 写 home/role=leader，需重启 fm-server 生效，自动选举延期）。fm-server `spawn_cluster(engine, role, set)` 角色注入消除 env 竞争。fm-cli `cluster status/promote`。验收：3 e2e 场景全绿（commit→catchup read-local 一致 / 增量同步 seq 推进 / leader 宕机→LeaderDown→promote→新 leader 续写）+ ReplaySink 覆盖测试 + fm-cluster 各文件离线 regions 91-100%。285 离线测试全绿，clippy/fmt clean。**离线总 regions 87.65%**（较 M4 90.63% 降，因新 fm-cluster crate + engine 集成扩 regions 分母，而 mlx-gated summarize/consolidate saga + engine_builder !stub 分支离线不可达；live 口径仍覆盖这些分支，PRD 验收以 live 为准，M6 未触 mlx 代码故 live regions 不变）。

**M5 部分完成**（降级定位，非阻塞主线）：PRD §14 三部分 — (a) store-fusion 可选切换、(b) `audit_memory_access` → fusion-guard DLP gate、(c) perf 基线 p99<50ms + 并发。**(c) 已落地**：轻量手写 bench（`crates/fm-engine/benches/retrieve_bench.rs`，无 criterion 重依赖），store-stub 10k 条记忆 + StubEmbedder dim=64，单条 retrieve p99=14.3ms（<50ms ✅）、10 并发 p99=140ms（<200ms ✅）。基线 JSON 落 `benches/baseline-2026-08-27.json`。**(a)(b) 降级**：见 M5 PRD 偏离记录。**(c) 之外的 R8/§10.4 PII 正则脱敏已落地**（此前零脱敏的真空补齐）：`fm-engine/src/redact.rs` 五类 PII 正则（phone/email/idcard/bankcard/ipv4，regex crate 无 lookaround，顺序敏感替换避误吞），占位符 `[REDACTED:type]`，幂等。commit/import 写入路径在 embed+persist 前脱敏，故向量/图谱/检索全用脱敏后内容。env `FUSION_MEMORY_REDACT_PII=1` 开启（`MemoryEngine::with_redact()` + fm-server/fm-cli 导入路径同源 env）。13 脱敏测试绿。验收：perf bench 两 gate pass + 13 脱敏测试绿 + 301 离线测试全绿 + live 测试全绿（bge-m3 + Qwen3.8-27B-4bit，实体抽取 JSON 100%）+ 离线 regions 90.82% / live regions 92.47%（均 ≥90%）+ clippy/fmt clean。

| 里程碑 | 内容 | 状态 |
|--------|------|------|
| M0 | workspace + 核心类型 + trait + CI | ✅ |
| M1 | store-stub 后端 + 引擎可跑（stub embedding）+ CLI | ✅ |
| M2 | 真实 embedding + 实体抽取 + 图 + 融合评分 + 导入 | ✅ |
| M3 | 服务化 + PyO3 + consolidate + 鉴权 | ✅ |
| M4 | 消费方接入 (in-scope ✅ / outward ✅) | ✅ |
| M5 | PII 脱敏 + perf 基线 + store-fusion/guard 降级 | ✅（部分） |
| M6 | 集群同步 leader-follower | ✅ |

### M2 PRD 偏离记录（Rule 7）

- **Kuzu DB → SQLite 递归 CTE**（裁定 2026-08-26）：PRD §9.2 选 Kuzu DB 嵌入图，但 Kuzu 无 Rust binding。改用 SQLite 递归 CTE（`relation` 表 + `WITH RECURSIVE` N-hop 遍历），`fm-persist` 内实现，`fm-graph::graph_affinity` 消费。功能等价（N-hop 可达性 + 直接命中），无需额外 server 进程。

### M5 PRD 偏离记录（Rule 7，降级裁定 2026-08-27）

PRD §14 M5 三部分，(a) store-fusion 可选切换、(b) fusion-guard DLP gate 降级为自带 PII 正则脱敏，(c) perf 基线已落地。

- **(a) store-fusion 切换 → 降级不实施**（裁定 2026-08-27）：PRD §14/Tech Selection 拟复用 `fusion-store`（HNSW）做零拷贝后端。但实测：① `fusion-store` 的 `FusionStoreEngine`（`fs-core/src/engine.rs`）与 fm-store 的同名 trait 是**两套不同 API**（方法集不同：create_vector_index/open_vector_index/columnar/checkpoint/recover，全签 `timeout: Option<Duration>`、返 `Result<bool>` vs fm-store 返 `MemoryResult<()>`），非同名 trait；② fusion-store 非 git 仓库、非本 workspace 成员，受"只能改本目录工程"约束无法作 path dep 消费；③ fm-store A4 已否定零拷贝（"放弃零拷贝幻象，get_vector 返回 owned Vec"）。故 store-stub 保持长期生产后端，store-fusion 切换不实施。perf gate 亦针对 store-stub（非 fusion-store），(c) 不受影响。
- **(b) fusion-guard DLP gate → 暂用自带 PII 正则脱敏，正式 gate 待上游补 PII 类**（裁定 2026-08-27，复核 2026-08-27）：PRD R8/§10.4 "M5 接 guard 做正式 DLP gate"。复核发现 **fusion-guard 已落地**（git 仓库 `dahai80/fusion-guard`，13 crate，`fg-audit-engine::AuditEngine` 真正 DLP gate + UDS JSON-RPC `guard.redact/evaluate/reveal/confirm` via `fg-ipc`，可 IPC 消费无需 Rust 依赖，契合 100% 离线）。但实测 `fg-redact::Redactor` 当前只覆盖**凭证类**（api_key/password/id_number/private_key），**不覆盖** fusion-memory 所需的 **PII 类**（phone/email/bankcard/ipv4）——覆盖面缺口。故暂留 fusion-memory 自带最小 PII 正则脱敏（`fm-engine/src/redact.rs`，五类 PII）作过渡。已向上游提 issue 跟踪：**fusion-guard #2**（请求 `fg-redact` 增 PII pattern classes，phone/email/bankcard/ipv4，含 order-sensitivity 顺序敏感替换 + 回归测试）。上游落地 PII 类后，fusion-memory 弃用 `redact.rs` 改走 UDS `guard.redact`（irreversible）正式 DLP gate，接入点不变（`with_redact()` builder + commit/import 写入路径）。
- **(c) perf 基线 → 已落地**：见 M5 总结段。两 gate 达标，基线 JSON 存档。

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
| `fm-cluster` | M6 集群同步：leader/follower 角色 + wop_log 复制 + TCP 传输 + 手动 failover |

## 构建

```bash
cargo check --workspace        # 编译检查
cargo test --workspace         # 全离线测试 (301 用例, 排除 fm-py cdylib)
cargo clippy --workspace --all-targets -- -D warnings   # lint
cargo fmt --all --check        # 格式检查

# §13.2 perf 基线 (store-stub 10k 条, StubEmbedder, 免模型):
#   cargo bench -p fm-engine --bench retrieve_bench
#   单条 retrieve p99<50ms + 10 并发 p99<200ms, 结果落 /tmp/fm-perf-baseline-*.json

# 真实模型集成测试 (需起 fusion-mlx 加载 bge-m3 + Qwen 聊天模型, 串行避 429):
#   ~/claude-home/fusion-mlx/start.sh start
#   scripts/live-test.sh            # 全 workspace live (串行)
#   scripts/live-test.sh fm-engine  # 单 crate
#   聊天模型默认 Qwen3.5-9B-4bit; 若未缓存可用 env 覆盖:
#   FUSION_MEMORY_CHAT_MODEL=Qwen3.8-27B-4bit scripts/live-test.sh   # 已验证可用
#   (Qwen3-0.6B 实体抽取太弱返空实体, 不推荐做 extract)
```

覆盖率（需 llvm-tools，系统 llvm 或 rustup 组件）：

```bash
# 离线默认（CI 口径）：排除 fm-py（PyO3 cdylib 绑定层，验收走 PyO3 往返，不走单测覆盖率）。
# regions 90.82%。301 用例全绿。
# 注：跑覆盖率前先 `cargo llvm-cov clean`，旧 profraw（含未触发的 bench 插桩二进制）会稀释 regions。
# engine.rs summarize/consolidate saga + engine_builder.rs !stub 分支离线不可达（走真 mlx LLM/embedding），
# PRD 验收口径 = live（覆盖这些分支）。
LLVM_COV=/opt/homebrew/opt/llvm/bin/llvm-cov \
LLVM_PROFDATA=/opt/homebrew/opt/llvm/bin/llvm-profdata \
cargo llvm-cov --workspace --summary-only --exclude-from-report fm-py --ignore-filename-regex "src/main\.rs"

# 真实模型集成（需起 fusion-mlx 加载 bge-m3 + Qwen3.5-9B-4bit，串行避 429）：
# ~/claude-home/fusion-mlx/start.sh start
LLVM_COV=/opt/homebrew/opt/llvm/bin/llvm-cov \
LLVM_PROFDATA=/opt/homebrew/opt/llvm/bin/llvm-profdata \
cargo llvm-cov --workspace \
  --features fm-embed/mlx-live --features fm-engine/mlx-live --features fm-server/mlx-live \
  --summary-only --exclude-from-report fm-py -- --include-ignored --test-threads=1
# live regions 92.47%（覆盖 !stub 真 mlx 分支 summarize/consolidate saga + engine_builder）。PRD 验收以此为准。
```

> **覆盖率口径**：以 regions 为准（业界标准 + PRD 无 functions 硬指标）。
> functions % 受 trait 单态化跨 binary 重复 0 计数假低（`FusionStoreEngine for StoreStub`
> 在每个 test binary 各实例化，未调实例记 0），非真未覆盖——stub.rs 全方法均有单测，
> `tests/offline_integration.rs` 实调 trait 路径。regions 不受此假象影响。
>
> **fm-py 排除**：PyO3 `extension-module` cdylib，macOS 需 `.cargo/config.toml` 的
> `-undefined dynamic_lookup` 延迟解析 Python 符号。验收 = PyO3 往返（见下），非单测覆盖率，
> 与 PRD §11.3 一致。
>
> **PyO3 往返验收**（GIL 安全，C2）：
> ```bash
> PYO3_PYTHON=/opt/homebrew/bin/python3.12 cargo build -p fm-py
> cp target/debug/libfusion_memory.dylib /tmp/fmpy/fusion_memory.so
> python3.12 roundtrip.py  # commit→2 ids / retrieve→block / consolidate→report / GIL 不冻结
> ```
> commit 期间另线程纯 Python 计数器持续增长 → 证明 `py.allow_threads` 释放 GIL，事件循环不冻结。

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

# M3: 遗忘/合并/摘要/对账 saga (PRD §5.6)
./target/release/fm --home ~/.fusion-memory consolidate           # 触发 saga, 报告 dropped/promoted/merged/summarized/reextracted/reconciled
./target/release/fm --home ~/.fusion-memory merges                # 列 merge_log (供 unmerge 查 id)
./target/release/fm --home ~/.fusion-memory unmerge --id 42       # 撤销合并: source 反 tombstone, 删 merge_log
./target/release/fm --home ~/.fusion-memory reconcile             # 跨库对账: tombstone 物理删, 悬空向量落 report
```

`--home` 默认 `~/.fusion-memory`（或 `FM_HOME` 环境变量），`--dim` 默认 64（stub）；真实 embedding 走 bge-m3 dim=1024，`import` 不用 `--stub` 时自动用 1024。
`import` 映射：tier short_term→Short / long_term→Long / archive→跳过；memory_type user→Semantic / feedback→Procedural / project→Episodic / reference→Semantic；scope `graph:NAME` 或 metadata.graph_id → Project 实体。

## 服务运行（M3）

```bash
cargo build --release -p fm-server   # 产出 target/release/fm-server

# start.sh 管理 (start/stop/restart/status/log/doctor)
./start.sh start      # 启 fm-server (默认真 bge-m3; FUSION_MEMORY_STUB=1 离线)
./start.sh stop       # 优雅停 (SIGTERM)
./start.sh status     # PID/端口/sock/内存/healthz
./start.sh doctor     # 健康检查: binary/端口/mlx 连通/data dir
./start.sh log        # tail 日志

# env (见 ServerConfig::from_env):
#   FM_HOME (默认 ~/.fusion-memory)
#   FUSION_MEMORY_HTTP_PORT (默认 11435) / FUSION_MEMORY_API_KEY (HTTP 必配, B5)
#   FUSION_MEMORY_STUB=1 (StubEmbedder 离线, 不连 mlx)
```

UDS JSON-RPC（sock 0600，B6）+ HTTP（axum 强制 Bearer，B5，端口 11435）并发。未配 `FUSION_MEMORY_API_KEY` 但 HTTP 端口开 → 拒启 HTTP（仅 UDS）。路由：`POST /v1/memory/{commit,retrieve,consolidate,audit,delete}`、`GET /v1/memory/{id}`、`GET /healthz`（公开）；`delete` 需 `params.confirm=true`。

## 约定

- 4 空格缩进，无 docstring（`//!` 模块文档 + 行内注释）
- `tracing` 日志，`anyhow`（应用）+ `thiserror`（库）
- 失败可见，不静默吞错
