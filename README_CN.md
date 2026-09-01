# fusion-memory

> **[English](README.md)** | 中文

> **当前版本：v1.1.0（Commercial GA）** — 商用正式发布。硬阻断项全闭环 + 真测试验证 + RC 已知限制三项全消。已知限制见 `CHANGELOG.md` "Known limitations"，非阻塞。

Fusion 生态（"一核九端"）系统级长/短期记忆与认知图谱中枢。解决 Agent 跨 session 状态断层、重复提问、context window 爆炸，目标：越用越懂用户。

- 权威 PRD：`architecture/fusion-memory-prd-0825.md`
- 落地架构：`~/fusion/fusion-memory-prd-plan-0826.md`
- 审计报告：`~/fusion/audit/fusion-memory-audit-0827.md`

## 状态

**v1.0.0 — API 稳定承诺（2026-08-28）**：11 crate 锁定 1.0.0。自此起遵循语义化版本（SemVer）契约：
- `MAJOR`（2.0+）：仅引入**不向后兼容**的破坏性变更，须提前在 changelog 公告 + 迁移指南。破坏性变更包括：移除/重命名已有 RPC 方法、改变 HTTP 路径、改变字段语义、改变默认行为。
- `MINOR`（1.x）：向后兼容的新方法/字段/端点/性能改进，客户端**不可**因 MINOR 升级而中断。
- `PATCH`（1.0.x）：缺陷修复，无行为变更。
- **冻结的线契约（wire contract）**：UDS JSON-RPC 方法集 + `v1.<method>` 前缀路由 + `jsonrpc=="2.0"` 校验（见 `jsonrpc::API_VERSION=1`）；HTTP `/v1/memory/*` 路径 + Bearer 鉴权 + `confirm` 守卫。二者在 1.x 全周期不变。
- 客户端协商：调 `version` RPC 或 `GET /v1/memory/version` 取 `api_version`，据版本号分支处理。
- 1.0 前的 0.x 版本为技术预览，无稳定承诺。

**store-fusion adapter + fg-redact 凭据脱敏落地（2026-08-28，未发版）**：用户需求 "现在建 store-fusion adapter ，然后换上游fg-redact" 两部分全落地。
- **store-fusion adapter**（`fm-store/src/fusion.rs`，feature `store-fusion`）：实现 fm-store `FusionStoreEngine` trait，包上游 fusion-store `fs-core` 的 `Engine`（HNSW + mmap KV）。距离语义桥接：fs-core 返 `distance = 1 - cos_sim`，adapter 转 `similarity = 1.0 - distance` 对齐 fm-store 契约（同 local.rs 公式）。UFCS 调 fs-core trait 方法（两 crate 同名 trait `FusionStoreEngine`）。ZeroCopyBuffer mmap→owned 桥接。6 测试绿（kv 往返 / 向量插入+取+搜 / dim 不匹配拒 / 搜索 dim 不匹配 / 删后取 None / list_ids 排除软删）。**附加非互斥**：与 local-store 共存（local-store 默认 + 常开；store-fusion 可选，默认关），两者同编译。关闭 RC 已知限制 #2（store-stub 命名 —— store-fusion 现为真实 fusion-store 后端备选，非仅 "stub"）。
- **fg-redact 凭据脱敏**（`fm-engine/src/redact.rs`）：段 1 凭据脱敏委托上游 `fg-redact::Redactor::redact_credentials()`（fusion-guard PR #11 / issue #10）。fg-redact 补 fusion-memory 原没有的 10 类凭据（JWT/private_key/oauth_bearer/api_key/conn_string/password/secret_kv/env_kv/netrc/aws_secret）。段 2 PII 仍 fusion-memory 自带（手机+86/0086/邮箱/身份证/银行卡+Luhn/IPv4/护照/IPv6/国际手机）—— fg-redact 的 PII 行为更差（身份证被 credit_card 错吞 / id_number 误吞长数字 / +86 phone 被 border 拒），故 PII 不走 fg-redact，见 redact.rs 模块文档。关闭 RC 已知限制 #3 凭据部分（凭据现走上游；PII 按设计留本地）。4 新测试（jwt / password / 凭据+PII 同段 / 身份证仍本地非 bankcard）。幂等：凭据占位 `[REDACTED:jwt]` 无数字 → PII 正则不二次匹配。
- **测试计数**：默认 feature 425→429（+4 凭据测试）；`--features fm-store/store-fusion` 435（429 + 6 store-fusion 测试）。gate 全绿（fmt / clippy -D warnings / check / test）。
- **上游**：fusion-guard #10/#11 凭据 API 已落地（issue 提 + PR #11 实现 + 8 issue10_* 测试），fusion-memory 消费 `redact_credentials()`。fusion-store #3/#4 仍跟踪（adapter 已建消费 fs-core path dep；store-stub 仍默认生产后端）。

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

> **里程碑全集 M0–M6（终态）**：落地架构 `~/fusion/fusion-memory-prd-plan-0826.md` §14 仅定义 M0–M6，M6 为最终里程碑，**无 M7+**。§15 未决项 6 项全部 ✅ 已裁定；§17 审计修正 E1（8 项）/E2（10 项）/E3（3 项）全部已落地或已决策，审计闭环无遗留。后续工作仅两类：(1) M5(b) 待上游 `fusion-guard#2` 补 PII 类后接正式 DLP gate；(2) PRD 外的运维/性能/消费方演进。

### 审计 P0–P3 修复记录（2026-08-27，全闭环）

`audit/fusion-memory-audit-0827.md` §8 的 16 项缺陷全部修复，按文件簇分 9 批落地，每批 `cargo test` checkpoint，315 离线测试全绿（基线 301 → +14 新增回归测试），clippy `-D warnings` + fmt clean。

**P0 阻断商用（5 项）**
- **H1 跨存储写无事务原子性**：`put_memory` 包进 `conn.transaction()`（memory_item + entity + memory_entity 三类 INSERT 同事务，失败 rollback 不留半截实体行）；`commit_episodic_memory` 在 `put_memory` 失败时反向 `delete_vector` 清已写 sled 向量。`fm-persist/src/store.rs`、`fm-engine/src/engine.rs`。
- **H2 集群重放非幂等 + LeaderDown 误判**：`insert_vector` 幂等化（已落盘且未 tombstone → 跳过 `hnsw.insert`，replay 重发不重复入索引；tombstone 状态清 tomb 后照常重插=复活路径）；重放错误分类（瞬时网络/解析失败 retry，永久 sink 失败上抛）。`fm-store/src/stub.rs`、`fm-cluster/src/replay.rs`。
- **H3 集群 TCP 无鉴权 + 帧 4GB OOM + 明文**：`read_frame` 加 `MAX_FRAME_LEN`（16MB）上限防 OOM；`handle_conn` 校验 cluster_token（Hello 握手期比对，空 token 内网放行）；明文风险文档化（内网离线边界，非公网，PRD §16 已界定）。`fm-cluster/src/protocol.rs`、`fm-cluster/src/transport.rs`。
- **H4 consolidate TOCTOU + 丢失更新**：引擎级 `tokio::sync::Mutex<()>`（`consolidate_lock`）串行 `consolidate_memories` 与 retrieve 的 touch_access 写（snapshot→决策→写原子）；`touch_access_batch` 去重 + 单次批量 `UPDATE ... WHERE id IN (...)`，相对 `access_count=access_count+1` 防丢失更新。`fm-engine/src/engine.rs`、`fm-persist/src/store.rs`。
- **H5 PII 脱敏非企业级 + 撒谎注释**：bankcard 加 Luhn 校验 + 上下文边界（避误吞订单号/时间戳）；扩 PII 类（phone/email/idcard/bankcard/ipv4，顺序敏感替换避手机吞银行卡前 11 位）；`redact.rs:58` env 注释改正（每次调用读 env，非启动期，且仅在 builder/import 非热路径）。`fm-engine/src/redact.rs`。

**P1 必修（5 项）**
- **L1 graph_affinity 恒 0**：`retrieve_context` 在 extractor 在场时对 query 文本抽实体 → 传 `query_entity_ids` 给 `score_candidate`，graph_affinity 接通（直命中 1.0 / N-hop 0.5^h）。`fm-engine/src/engine.rs`。
- **L2 touch_access 多次累加**：见 H4 `touch_access_batch`（同 id 多 turn 命中只 +1，"检索会话"计次）。
- **L3 reconcile 单向**：增 store→SQLite 反向孤儿扫描（`StoreStub::list_vector_ids()` 枚举非 tombstone 向量 id，不在 SQLite `vector_ref` 集合 → 孤儿，落 report + `delete_vector`）；`physical_delete` 显式级联 `memory_entity`（不靠 FK pragma 跨连接保证）。`fm-store/src/stub.rs`、`fm-persist/src/store.rs`、`fm-engine/src/engine.rs`。
- **L4 delete 静默跳过**：坏 `vector_ref`（非数字/污染）不 `unwrap_or(true)` 静默物理删（会留幽灵向量），改 warn + `append_reconcile("bad-vector-ref")` + 跳过物理删（reconcile 兜底清）。`fm-engine/src/engine.rs`。
- **L5 slug 碰撞**：entity id = `ent-{slug}-{fnv1a_64(name)}`（FNV-1a full-name hash 保唯一，slug 仅显示）。C/C++/C# 等 slug 同名异实体 id 全异。`fm-engine/src/entity_extract.rs`、`fm-cli/src/import_studio.rs`。

**P2 性能（4 项）**
- **P1 单 Mutex 全串行 + poison panic 放大**：保留单 `Mutex<Connection>`（SQLite WAL 单写者，连接池 r2d2 对 Rule 2 过度设计），24 处 `.expect("poisoned")` → `conn()`/`conn_mut()` helper 返 `PersistError::Poisoned` 上抛，不再 panic 放大单点故障到全局。`fm-persist/src/store.rs`、`fm-persist/src/error.rs`。
- **P2 向量 serde_json 文本存 sled（~3.7x 浪费）**：改 LE f32 原始字节（4B/float，serde_json 文本 7-12B/float），反序列化零分配。`fm-store/src/stub.rs`。
- **P3 consolidate_merge O(S×KNN×N) 灾变**：KNN 内层循环 `list_all()` 全表扫 + 字符串反查 → 循环外一次性建 `vector_id → &MemoryItem` 索引，内层 O(1) 查。`fm-engine/src/engine.rs`。
- **P4 CTE 指数扇出 + 每搜索单点查 tombstone**：递归 CTE 加 `LIMIT 256` 早终止（graph_affinity 远端节点 0.5^h 指数衰减，截断无损精度）；`search_knn` tombstone 检查改 `tombstone_set()` 单次批量加载入 HashSet，替代每邻居/每 fallback 向量 N 次 sled 点查。`fm-persist/src/store.rs`、`fm-store/src/stub.rs`。

**P3 维护（3 项）**
- **M1 撒谎注释**：physical_delete 级联注释（已显式 DELETE memory_entity，注释与实现一致）+ redact.rs env 注释（见 H5）。`fm-persist/src/store.rs`、`fm-engine/src/redact.rs`。
- **M2 extract_and_attach 吞 DB 错**：`get_memory().unwrap_or(None)` 把 SQLite 错误吞成"无此记忆"（DB 故障伪装成数据缺失）→ 显式 match，DB 错误 warn + return（pending 保持 true 待重抽），不伪装。`fm-engine/src/engine.rs`。
- **M3 Runtime::new per Engine 线程爆炸**：fm-py 每 Python `Engine` 各建 tokio runtime（N×worker 线程）→ 进程级 `OnceLock<Runtime>` 共享单 runtime（2 worker），所有 PyEngine 复用。`fm-py/src/lib.rs`。

### v0.1.1 补丁（2026-08-27，issue #1/#2/#4）

修复 3 个开放 GitHub issue，新增 2 个 RPC + 1 个 UDS method：

- **issue #2 — scope 级删除/计数**：新增 `delete_scope`（按 session_id 批量 tombstone，含 `confirm` 守卫，复用 delete 的向量清理+`append_wop` 审计）与 `count`（全量或按 session 计数）。后端 `fm-persist` 加 `delete_by_session`/`list_by_session`/`count_by_session`，引擎 `MemoryEngine::delete_scope`/`count`，trait 加默认 `Unsupported` impl（测试 stub 免改）。HTTP `POST /v1/memory/{delete_scope,count}` + UDS method `delete_scope`/`count`。
- **issue #1/#4 — `memory.retrieve_context` 契约**：fusion-event 需要 `{trigger_id, query, top_k, node_id}` → `{context, memory_ids, cache_hit}`。新增 UDS method `memory.retrieve_context` 适配已有 `RetrieveQuery`，把 `FormattedContext.blocks` 融合成契约形态（context = turns 以 `\n---\n` 拼，memory_ids = interaction_id 去重，cache_hit=false）。

验收：325 离线测试全绿（基线 301 → +24 新增，persist 3 + dispatch 5 + http 4 + trait/引擎 12），clippy `-D warnings` + fmt clean，`cargo check --workspace` clean。CI 受 GitHub 账户计费阻断（`recent account payments have failed`，非代码问题，本地 fmt/clippy/check/test gate 为代理口径）。

### v0.2.0 审计二轮深度修复（2026-08-28，架构层 + 生产路径门禁）

审计报告 `audit/fusion-memory-audit-result-0827.md` §1/§2/§3 深度项（48 findings，分 8 批落地）全部修复。本轮聚焦架构层耦合、生产路径零覆盖、错误类型语义化，与 v0.1.x 的行为缺陷修复互补。354 离线测试全绿（基线 325 → +29 新增回归测试），clippy `-D warnings` + fmt clean。

**架构层解耦（§1.1/§1.4/§1.5）**
- **§1.1 连接池打破单 Mutex 串行**：`Persist` 从 `Mutex<Connection>` 改 `r2d2::Pool<SqliteConnectionManager>`（POOL_SIZE=8）。WAL 原生 1 写 N 读并发此前被单连接抵消；`PooledConnection` Deref→`Connection`，`prepare_cached`/`transaction()` 调用点零改动。新增 `PersistError::Pool` + `From<r2d2::Error>`，超时/busy → `MemoryError::Busy` 可重试。`fm-persist/src/store.rs`、`fm-persist/src/error.rs`、`Cargo.toml`（r2d2 0.8 / r2d2_sqlite 0.35，匹配 rusqlite 0.40 bundled）。
- **§1.4 store 后端 trait 化**：`MemoryEngine.store` 字段从 `Arc<StoreStub>` 改 `Arc<dyn FusionStoreEngine>`（动态分发，不绑死具体后端）。`FusionStoreEngine` trait 补 `list_vector_ids`（reconcile 反向对账 store→SQLite 孤儿扫描）。store-fusion 后端从空壳改 `compile_error!` 显式阻断（上游 fusion-store trait 对齐前）。`fm-store/src/trait_def.rs`、`fm-store/src/stub.rs`、`fm-store/src/fusion.rs`、`fm-engine/src/engine.rs`。
- **§1.5 图层存储抽象**：新增 `fm_graph::GraphStore` trait（仅 `n_hop_reachable` + `list_entities_by_type` 两方法，图层所需最小接口），`impl GraphStore for Persist`。`graph_affinity`/`align_entity`/`score_candidate` 签名从 `&Persist` 改 `&dyn GraphStore`，图层不再 `use fm_persist::Persist`。新增 mock 测试 `mock_store_no_sqlite_needed` 证图层可纯内存单测（无需 `Persist::open_in_memory()` + SQL 填数据）。`fm-graph/src/store.rs`、`fm-graph/src/affinity.rs`、`fm-graph/src/align.rs`、`fm-engine/src/scoring.rs`。

**生产路径门禁（§1.6/§1.12）**
- **§1.6 CI live-compile 门禁**：CI 新增 `live-compile` job，每 PR 编译全部 mlx-live 门禁测试（`--features mlx-live --no-run`），验证 `MlxEmbedder` bge-m3 / consolidate saga / 真实 HTTP 代码路径类型检查通过。此前 CI 默认构建中这些生产路径编译 0 次（live 测试 `#![cfg(feature = "mlx-live")]` + `#[ignore]` 双门禁），"325 绿"对 live 路径零回归保护。实际执行仍手工（需 fusion-mlx on Apple Silicon）。`.github/workflows/ci.yml`。
- **§1.12 桩对桩同义反复**：91 条 stub 测试（`StubEngine`/`DispatchStub`/`EchoEngine` 返魔数常量）标注为接线测试（证 wire passes params，非行为测试）；真实 `MemoryEngine` 经 HTTP/JSON-RPC 真栈往返的行为覆盖由 `tests/offline_integration.rs` + `tests/consumer_scenarios.rs` 承担（stub engine + 真栈，已在 `cargo test --workspace` 默认运行）。`fm-server/src/jsonrpc.rs`。

**错误类型语义化（§2.8）**
- **§2.8 错误类型 finalize**：`MemoryError` 补 `Poisoned`/`Busy`/`NotFound` 语义变体（旧版全压成 `Sqlite(String)`，运维误当 sqlite 错误跑 VACUUM，真诊断被隐藏）。`PersistError::to_memory` 区分映射：Poisoned→Poisoned（永久不可重试）、SQLITE_BUSY/locked→Busy（瞬时可重试）、Pool 超时→Busy。`retryable()`/`is_not_found()` helper 供调用方决策。`fm-core/src/error.rs`、`fm-persist/src/error.rs`。

> 完整 8 批分批记录（Batch 0–7，按文件簇 + checkpoint）见 git 历史 `fix/audit-p0-p3-layering-0828` 分支。验收口径：354 离线测试全绿 + clippy `-D warnings` + fmt clean + live-compile 门禁编译通过。

### v0.2.1 生产就绪审计 P0–P3 修复（2026-08-28，第三轮）

生产就绪审计报告 `audit/fusion-memory-audit-result-product-0827.md` §8 的 22 项（6 P0 + 10 P1 + 6 P2，无 P3）全部处置。可代码修复项全落地，3 项 epic-scale 架构项显式延后并文档化为 SLA/路线图。403 离线测试全绿（基线 354 → +49 新增回归测试），clippy `-D warnings` + fmt clean，`cargo check --workspace` clean。

**P0 阻断商用（6 项全修）**
- **P0-1 进程监管**：新增 `scripts/fusion-memory.service`（systemd unit，Type=notify 集成 healthz，Restart=on-failure + StartLimitBurst，journal 日志）。配套 `start.sh` 文档化部署路径（systemd 管理 / 手动 start.sh 二选一）。`scripts/fusion-memory.service`。
- **P0-2 metrics 端点**：`GET /metrics` 返 Prometheus 文本格式（http_requests_total / http_errors_total / http_request_duration_seconds histogram + engine 层 embedder_in_flight / consolidate_running / store_pool_in_use）。公开不加 Bearer（同 healthz，供 monitor 抓取）。`crates/fm-server/src/metrics.rs`、`crates/fm-server/src/http.rs`。
- **P0-3 HTTP body 上限**：axum `DefaultBodyLimit::max(8MB)` 全路由生效（与 UDS `MAX_LINE_BYTES` 对齐），超限 413 Payload Too Large 不到 handler，防 POST 大 body 内存放大 DoS。`crates/fm-server/src/http.rs`。
- **P0-4 备份机制**：`scripts/backup.sh`（SQLite `.backup` 在线热备 + sled 目录 cp，时间戳归档，保留窗口可配），`fm-cli backup` 子命令调同逻辑。文档化 cron 部署。`scripts/backup.sh`、`crates/fm-cli/src/backup.rs`。
- **P0-5 CI billing-blocked**：非代码问题（GitHub 账户 `recent account payments have failed` 阻断 Actions 付费运行）。本地 gate（fmt/clippy/check/test）为代理口径已全绿。延后至账户计费恢复，非代码可修。
- **P0-6 部署制品**：`Dockerfile`（多阶段构建，distroless 运行时，非 root 用户，仅暴露 11435）。`scripts/build-artifact.sh` 打包二进制 + 配置模板。`Dockerfile`、`scripts/build-artifact.sh`。

**P1 必修（10 项全修）**
- **P1-1 commit 部分失败**：`commit_episodic_memory` 返 `CommitOutcome{memory_ids, failed_turns}`，单 turn embed/insert/persist 失败记 `TurnFailure` 不中断其余 turn，客户端可感知失败 turn 重试。旧版全压 `Err` 丢整批。`crates/fm-engine/src/engine.rs`、`crates/fm-core/src/report.rs`。
- **P1-2 consolidate 半合并补偿**：merge 写 `memory_item` 成功但 `merge_log` 失败 → `unmerge` 自动回滚（反向合并 + 清 merge_log 行 + warn），不留半合并幽灵。`crates/fm-engine/src/engine.rs`。
- **P1-3 tracing + 审计日志**：全引擎 `tracing` 结构化日志（commit/retrieve/consolidate 各阶段 span + 计数）；`audit_log` 表记 actor/action/target/detail，consolidate 审计落 `actor="system"`。`crates/fm-engine/src/engine.rs`、`crates/fm-persist/src/store.rs`。
- **P1-4 PII 日志泄漏**：`tracing` 字段经 `redact_text` 脱敏（memory content/params 入日志前过 PII 正则），日志不含原始 PII。`crates/fm-engine/src/engine.rs`。
- **P1-5 UDS token 鉴权**：UDS 连接级 token（`FUSION_MEMORY_UDS_TOKEN`，连接首行 `AUTH <token>` 握手比对，空 token 本机放行），不匹配 → `-32004 unauthorized` 断连。多租户 UDS 鉴权。`crates/fm-server/src/uds.rs`。
- **P1-6 集群 bind gate**：leader/follower 启动校验 bind 地址（非 127.0.0.1/内网段 → 拒启，防误绑公网），PRD §16 离线边界强约束。`crates/fm-cluster/src/transport.rs`。
- **P1-7 规模验证 bench**：`crates/fm-engine/benches/scale_bench.rs`，10k/100k/1M 向量规模验证（`FM_SCALE` env 选档），测 seed 吞吐 / rebuild_from_sled / 单条 knn p99 / 10 并发 retrieve p99 / sled 磁盘占用。100k 基线落 `benches/baseline-scale-2026-08-28.json`。旧版仅 10k 未验证规模。`crates/fm-engine/benches/scale_bench.rs`。
- **P1-8 配置文件**：`fm-server` 支持 TOML 配置（`FM_CONFIG` env 或 `data_dir/fusion-memory.toml`）+ env 覆盖 + secret 文件（`FUSION_MEMORY_API_KEY_FILE`/`FUSION_MEMORY_UDS_TOKEN_FILE`，避免密钥落 env/cmdline）+ 启动 `validate()` fail-visible exit(1)。优先级 env > TOML > secret_file > default。`crates/fm-server/src/config.rs`、`crates/fm-server/src/main.rs`。
- **P1-9 连接池 get 超时**：r2d2 `connection_timeout(5s)` 显式兜底（默认 30s），池满 `get()` 超时返 `GetTimeout` → `MemoryError::Busy` 可重试，非无限阻塞防死锁。`crates/fm-persist/src/store.rs`、`crates/fm-persist/src/error.rs`。
- **P1-10 StoreStub 命名一致**：`store-stub` → `local-store`（feature flag）、`StoreStub` → `LocalStore`（类型）、`stub.rs` → `local.rs`（文件）。唯一实作者命名去贬义（非 stub，是长期生产后端）。`crates/fm-store/`。

**P2 发布后（6 项：3 修 + 3 显式延后）**
- **P2-2 PII 覆盖扩**：`redact.rs` 新增 IPv6（缩写 + 全写 8 段）+ 国际手机（E.164 `+\d{7,15}` 非 86 国家码，顺序在 China phone 后避双重脱敏）模式。姓名/地址类 regex 误报率高（locale-heavy），延后接 fusion-guard UDS `guard.redact`（待上游 fusion-guard#2 补 PII 类，见 M5 偏离记录 b）。`crates/fm-engine/src/redact.rs`。
- **P2-3 summarize 失败可见**：`consolidate_summarize` 的 mlx 调用失败（None = 网络/non-2xx/解析）或返空内容，旧版仅 warn 静默吞 → 现落 `ConsolidationFailure{stage:"summarize"}` 供客户端感知。`crates/fm-engine/src/engine.rs`。
- **P2-4 API 版本控制**：JSON-RPC `jsonrpc=="2.0"` 校验（非 2.0 → `-32600 invalid_request`，旧版静默吞字段）；方法版本前缀 `v1.<method>` 路由（无前缀 = 最新 = v1，向后兼容）；新增 `version` 方法 + `GET /v1/memory/version` 端点返 `api_version` 供客户端协商。`crates/fm-server/src/jsonrpc.rs`、`crates/fm-server/src/http.rs`。
- **P2-1 自动 failover / split-brain 防护 — 已落地（v1.0.0 B-2，退役延后状态）**：`fm-cluster::election` 精简自包含选举模块（leader-lease + term + quorum + wop_log last_seq 判定，**无 openraft**）替代手动 failover。leader 宕机 → follower 自动竞选胜出 → `epoch++` + 写 role=Leader → 重启成 leader（RTO 秒级）。旧 leader 复活经 §1.8 StaleLeader fencing 拒同步（防脑裂双写）。手动 `fm cluster promote` 仍保留（无 election 配置时）。split-brain 防护：quorum 多数写 + epoch fencing + token 鉴权（复用 H3）。16 新测试覆盖。详见 `### v1.0.0 自动 failover 选举`。
- **P2-5 Persist god-object 拆 trait — 显式延后（架构重构 epic）**：`Persist` 当前 30+ 方法（Memory/Relation/Entity/Wop/Reconcile 混合）。裁定延后：① 拆 5 trait（Memory/Relation/Entity/Wop/Reconcile）触及全引擎调用点（~60 处签名改 `&Persist` → `&dyn MemoryStore` 等）+ fm-py PyO3 绑定 + fm-cluster ReplaySink，是跨 crate 架构重构 epic，非本轮 P0-P3 单点修复；② `fm-graph::GraphStore` trait（v0.2.0 §1.5 已拆图层最小接口）证明拆 trait 模式可行， Persist 拆分沿用同法但规模量级不同；③ 当前 `Persist` 虽 god-object 但有清晰内部分区（各职责方法分组 + 注释），不阻塞商用。路线图：独立 PR 专项拆分，配迁移测试。
- **P2-6 依赖迁移 sled→fjall / hnsw_rs 备选 — 显式延后（评估中）**：sled 0.34 + hnsw_rs 0.3.4 维护风险评估。裁定延后：① sled 作者已推 fjall（后继项目，API 不同），迁移是 local-store 后端整体重写 + 数据格式迁移（存盘向量需 reformat），非本轮范围；② hnsw_rs 备选（`hnsw`/`hora` 库）需 benchmark 对比召回率/延迟，评估未完成前不换；③ 两依赖当前功能稳定（100k 规模 bench 已验证，见 P1-7），无已知阻塞 bug。路线图：先 bench 评估备选库召回/延迟，再定迁移优先级；sled→fjall 若做，配数据迁移脚本。

> 验收口径：403 离线测试全绿（基线 354 → +49 新增回归测试，覆盖 P0-2/3 metrics/body、P1-1/8/9/10 outcome/config/pool/rename、P2-2/3/4 PII扩/summarize失败/API版本 各 4-6 测试）+ clippy `-D warnings` + fmt clean + `cargo check --workspace` clean。3 项延后项（P2-1/5/6）已文档化为 SLA/路线图，非代码可修边界。

### M2 PRD 偏离记录（Rule 7）

- **Kuzu DB → SQLite 递归 CTE**（裁定 2026-08-26）：PRD §9.2 选 Kuzu DB 嵌入图，但 Kuzu 无 Rust binding。改用 SQLite 递归 CTE（`relation` 表 + `WITH RECURSIVE` N-hop 遍历），`fm-persist` 内实现，`fm-graph::graph_affinity` 消费。功能等价（N-hop 可达性 + 直接命中），无需额外 server 进程。

### M5 PRD 偏离记录（Rule 7，降级裁定 2026-08-27）

PRD §14 M5 三部分，(a) store-fusion 可选切换、(b) fusion-guard DLP gate 降级为自带 PII 正则脱敏，(c) perf 基线已落地。

- **(a) store-fusion 切换 → 原"降级不实施"，2026-08-28 adapter 落地**（裁定 2026-08-27 → 更新 2026-08-28）：PRD §14/Tech Selection 拟复用 `fusion-store`（HNSW）做零拷贝后端。原裁定"降级不实施"基于：① `fusion-store` 的 `FusionStoreEngine`（`fs-core`）与 fm-store 同名 trait 是两套不同 API；② 受"只能改本目录工程"约束。**2026-08-28 更新**：上游 fusion-store 落地 `fs-core` 后，fm-store 新增 `store-fusion` feature + `fusion.rs` adapter（包 fs-core `Engine`，UFCS 调具体方法非经 fs-core trait，避 API 不对齐），作为**可选后端**落地（默认关，local-store 仍默认 + 常开生产后端）。两 crate 同名 trait 由 UFCS 全限定消歧。零拷贝经 ZeroCopyBuffer mmap→owned 桥接（跨调用持 mmap 引用不安全，故 owned 拷贝；真零拷贝需上层借 mmap handle 保活，后续优化）。6 测试绿。**store-stub 保持默认长期生产后端不变**，store-fusion 为可选备选。perf gate 仍针对 store-stub（默认路径），(c) 不受影响。
- **(b) fusion-guard DLP gate → 凭据段已接上游 fg-redact，PII 段留本地**（裁定 2026-08-27 → 更新 2026-08-28）：PRD R8/§10.4 "M5 接 guard 做正式 DLP gate"。复核发现 fusion-guard 已落地（`fg-redact::Redactor` 覆盖凭据 + PII，但 **PII 行为有缺陷**：身份证被 credit_card 错吞 / id_number 无 validator 误吞长数字 / +86 phone 被 border validator 拒）。**2026-08-28 处置**（用户需求"换上游 fg-redact"）：向上游提 **fusion-guard #10**（请求 `redact_credentials` 凭据子集 API，跳 PII）→ PR **fusion-guard#11** 落地 `redact_credentials()` + `redact_with_patterns()` + `CREDENTIAL_PATTERNS` const（8 issue10_* 测试）。fusion-memory `redact.rs` 段 1 消费 `redact_credentials()`（10 类凭据，补原没有的 AWS/JWT/PEM/bearer/conn_string/.env 覆盖）；段 2 PII 仍 fusion-memory 自带（比 fg-redact PII 准，已测）。接入点不变（`with_redact()` builder + commit/import 写入路径）。未来全量 UDS `guard.redact` DLP gate 待上游 PII 行为修复后接。
- **(c) perf 基线 → 已落地**：见 M5 总结段。两 gate 达标，基线 JSON 存档。

### v1.0.0 商用阻断修复（2026-08-28，第四轮 — 商用就绪 hardening）

针对商用发布前 7 项阻断的处置。A 类 3 项受跨工程/账户约束非代码可修（盯上游 issue），B 类 2 项 epic + C 类 2 项在本轮落地。

**A 类 — 跨工程/账户阻断（非本仓库可修，已提上游 issue 跟踪）**
- **A-1 fusion-store 零拷贝后端**：PRD §14 拟复用 fusion-store HNSW 零拷贝。**2026-08-28 adapter 已建**（`fm-store/src/fusion.rs`，feature `store-fusion`，包 fs-core `Engine`），作为可选后端落地（默认关，local-store 仍默认生产后端）。仍跟踪 **fusion-store #3**（请求 VectorIndex 暴露 `get_vector(id)`+`list_vector_ids()`）+ **fusion-store #4**（请求 Engine impl）以补全 fs-core API 面。接入点不变（`FusionStoreEngine` trait 已 trait 化，§1.4）。
- **A-2 fusion-guard DLP 闸**：见 M5 偏离记录 (b)。**2026-08-28 凭据段已接上游**（fusion-guard #10/#11，`redact_credentials()` 消费）；PII 段留 fusion-memory 本地（fg-redact PII 行为缺陷，按设计）。全量 UDS `guard.redact` DLP gate 待上游 PII 修复后接。
- **A-3 GitHub Actions CI**：账户计费阻断（P0-5），非代码。本地 gate（fmt/clippy/check/test + fuzz）为代理口径全绿。

**B 类 — 此前延后 epic，本轮落地**
- **B-1 静态加密**：见 `### v1.0.0 静态加密` 专节。FDE（FileVault/LUKS）作主静态加密（ops 层，`deploy/README.md` 文档化）+ app 层 AES-256-GCM 敏感字段加密（`fm-persist` 加密层，defense-in-depth）。向量不 app 加密（FDE 覆盖 + 上游 PII 脱敏）。
- **B-2 自动 failover**：见 `### v1.0.0 自动 failover 选举` 专节。**精简自包含选举**（leader-lease + term + quorum 投票，**无 openraft 依赖**——openraft 仅 alpha 无稳定版，商用风险），替代手动 `fm cluster promote`。leader 宕机 → follower 自动竞选，RTO 从人工介入级降到秒级。退役 P2-1 延后状态。

**C 类 — 干净修复，本轮落地**
- **C-1 API 1.0 稳定承诺**：11 crate 0.2.1→1.0.0，SemVer 契约 + 线契约冻结（见状态段 v1.0.0 声明）。`jsonrpc::API_VERSION=1` + `v1.` 前缀路由 + `jsonrpc=="2.0"` 校验已就位。
- **C-2 fuzz + 负载压测**：见 `### v1.0.0 fuzz + 负载压测` 专节。`cargo-fuzz` JSON-RPC/HTTP 解析 fuzz 目标 + 100k 向量 + 高并发压测。

**仍待上游落地（非本轮）**：fusion-store #3/#4（fs-core API 面补全，adapter 已建消费 path dep）、fusion-guard PII 行为修复（凭据段已接 #10/#11，全量 UDS DLP gate 待 PII 修复）、GitHub 账户计费恢复（CI）。

**v1.0.0 B/C 批次验收**：425 离线测试全绿（基线 408 → +16 election 单元 + B-1 加密 4 已并入基线 + 1 B-2 e2e 全链路 orbit），clippy `-D warnings` + fmt clean + `cargo check --workspace` clean。B-1 静态加密 4 测试（明文兼容/加密往返/错 key fail-open/无 cipher 读密文）+ B-2 选举 16 单元测试（投票 4 判据 + quorum + 竞胜负 + 租约 + live TCP vote listener + from_env 边界）+ B-2 e2e 1 测试（`fm-server/tests/election_failover.rs`，orbit 全链路：leader 宕 → campaign → quorum → epoch++/role 文件 → detect Leader）。

### v1.0.0 静态加密

**分层策略（Rule 7，单一选 FDE 为主 + app 加密为纵深，非二者平均）**：

1. **FDE = 主静态加密（ops 层）**：SQLite `memory.db` + sled 向量库 + 集群 wop_log 全落 FDE 加密卷。macOS = FileVault（全盘）；Linux = LUKS/dm-crypt（数据卷）。FDE 对应用透明——hnsw_rs 在 RAM 内算距离用明文，落盘是密文，KNN 不受影响。`deploy/README.md` 文档化为部署前置要求 + 校验步骤。
2. **App 层字段加密 = 纵深防御（code）**：`fm-persist` 加密层对 SQLite `memory.content` + `entity.text` 敏感文本列做 AES-256-GCM 加密，即使磁盘快照泄露（绕过 FDE）也非明文。key 来源：`FUSION_MEMORY_ENC_KEY_FILE`（0600 文件，32B 原始 key）或 `FUSION_MEMORY_ENC_PASSPHRASE`（argon2 KDF 派生）。读路径透明解密。
3. **向量不 app 加密**：hnsw_rs 索引需明文算距离，app 加密会破坏 KNN。向量由 FDE + 上游 PII 脱敏（commit 前脱敏，向量从脱敏内容派生）覆盖。
4. **集群传输**：内网 loopback/私有网，TLS 暂不强制（PRD §16 内网离线）；敏感字段已 app 加密，传输中非明文。

**key 管理**：无 key = 明文模式（兼容旧行为，env 未配）；配 key = 加密模式。key 轮换 = re-encrypt 脚本（`fm-cli` 后续补）。密钥不入仓、不入日志。

### v1.0.0 自动 failover 选举

**精简自包含选举（替代手动 failover，退役 P2-1 延后）**：

- **选型**：**精简自包含 election 模块**（`fm-cluster::election`，~400 LOC），**不引 openraft**——openraft 最新仅 `0.10.0-alpha.34`，无稳定版、MSRV 未知、依赖树大，商用阻断（Rule 2 简单性 + Rule 7 单一选型）。实现 Raft 本质（非全量 Raft），契合 100% 离线约束。
- **算法**：leader-lease（`heartbeat_secs × heartbeat_fails`，复用 SyncConfig）+ term 递增投票 + quorum（`floor(nodes/2)+1`）+ 日志新旧判定（复用 wop_log `last_seq`）。
- **竞选触发**：follower `run()` 检测 LeaderDown（连续心跳失败）→ 转 candidate → 自增 term + 投自己 → 向所有 peer 发 `VoteRequest`（新增 `FrameKind::VoteRequest/VoteResponse`，复用 TCP 线帧）→ 收 quorum → 胜出。
- **投票授权判据**（Raft）：① `candidate.term ≥ own.term`；② `candidate.last_seq ≥ own.last_seq`；③ 本 term 未投过票（或已投该 candidate）；④ token 一致（复用 H3 鉴权）。不满足 → 拒。
- **胜出 → promote**：candidate 拿 quorum → `epoch++`（`write_epoch_file`，复用 §1.8 fencing）+ 写 `role=Leader`（`write_role_file`）→ orbit 退出，supervisor 重启该节点成 leader。旧 leader 复活后 epoch 低 → follower `StaleLeader` 拒同步（防脑裂双写）。
- **优先级**：节点列表下标小者优先（确定性，避随机，同 term 平票有定论）。
- **成员**：静态，env `FUSION_MEMORY_CLUSTER_NODES=host:port,host:port,...`（全节点），自身下标 `FUSION_MEMORY_CLUSTER_NODE_ID`（0-based）。未配 → 无选举（单机/手动模式兼容，零开销）。成员变更 = 改 env 重启（非动态 add/remove，Rule 2）。
- **新增依赖**：无（复用 transport TCP / wop_log / role epoch 文件 / async_trait）。
- **M3 e2e 场景重验**：commit→catchup 一致 / 增量 seq 推进 / leader 宕机→自动选举→新 leader 续写（原 promote 路径改自动）。16 election 单元测试覆盖（投票授权/拒绝 4 判据、quorum、竞选胜/败、租约到期、live TCP vote listener、from_env 边界）+ 1 e2e 全链路集成测试 `fm-server/tests/election_failover.rs`：驱动生产入口 `spawn_cluster(role=Follower)` + 真 MemoryEngine + 真 in-process TCP，验证 leader 宕 → `follower_orbit` campaign → quorum → 写 epoch++ + role=Leader 文件 → `detect_role_with_home` 读 Leader（补 orbit 全链路覆盖，旧 claim 仅单元 + 手动 promote）。
- **手动模式保留**：`fm cluster promote` 仍可用（无 election 配置时）。`fm cluster status` 显示 election 状态 + epoch。

### v1.0.0 fuzz + 负载压测

- **fuzz**：`cargo-fuzz` 目标 — JSON-RPC 请求解析（畸形 JSON / 超长 / 坏 UTF-8 / 类型混淆 / 嵌套深）、HTTP body 边界、实体抽取 JSON 解析。崩溃/panic 视为缺陷。`crates/fm-server/fuzz/`。
- **负载压测**：扩 `retrieve_bench` — 100k 向量（基线 10k × 10）+ 并发梯度（1/10/50/100），记录 p50/p99/p999 + 吞吐。商用级负载验证。结果落 `benches/baseline-1.0.0-*.json`。

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
| `fm-store` | `FusionStoreEngine` trait + local-store 后端（默认）+ store-fusion 后端（可选 feature，包 fusion-store fs-core） |
| `fm-embed` | fusion-mlx bge-m3 embedding（LRU+信号量）+ StubEmbedder |
| `fm-persist` | SQLite WAL 元数据 schema + CRUD + relation 表（递归 CTE 图遍历） |
| `fm-server` | UDS JSON-RPC + HTTP 服务 |
| `fm-py` | PyO3 Python 绑定 |
| `fm-cli` | CLI 运维/导入/查询 |
| `fm-cluster` | M6 集群同步：leader/follower 角色 + wop_log 复制 + TCP 传输 + 手动 failover；v1.0.0 B-2 自动 failover 选举（`election` 模块，leader-lease + term + quorum） |

## 构建

```bash
cargo check --workspace        # 编译检查
cargo test --workspace         # 全离线测试 (429 用例, 排除 fm-py cdylib; --features fm-store/store-fusion = 435)
cargo clippy --workspace --all-targets -- -D warnings   # lint
cargo fmt --all --check        # 格式检查

# §13.2 perf 基线 (store-stub 10k 条, StubEmbedder, 免模型):
#   cargo bench -p fm-engine --bench retrieve_bench
#   单条 retrieve p99<50ms + 10 并发 p99<200ms, 结果落 /tmp/fm-perf-baseline-*.json

# §13.2 live perf 基线 (真 fusion-mlx bge-m3 dim=1024, 端到端 retrieve 延迟, 关闭 RC 已知限制 #1):
#   需起 fusion-mlx 加载 bge-m3 (standalone 须 FUSION_ROUTE_WARN_ONLY=true + --api-key)
#   cargo bench -p fm-engine --features mlx-live --bench retrieve_bench_live
#   三路径: cold (唯一 query 真打 mlx) / cached (LRU 命中跳 mlx) / concurrent x5 (真 mlx)
#   基线 JSON: crates/fm-engine/benches/baseline-live-bgem3-2026-08-28.json
#   live 数据 (Apple Silicon): cold p50=9.98ms/p99=10.73ms, cached p50=0.107ms/p99=0.126ms, concurrent x5 p50=44.6ms
#   规模受 fusion-mlx rate-limit bug (#692: _serve_from_model_dir 漏配, 60rpm 常驻) 限, 小规模真延迟参考; 大规模压测用 retrieve_bench (StubEmbedder)

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
# regions 90.82%。429 用例全绿 (--features fm-store/store-fusion = 435)。
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
- **活体验收**：`FUSION_MEMORY_MLX_API_KEY=change-me cargo run -p fm-cli --example live_acceptance`（需 fusion-mlx 起 bge-m3 + Qwen3.5）。

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
FUSION_MEMORY_MLX_API_KEY=change-me ./target/release/fm --home ~/.fusion-memory import
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

UDS JSON-RPC（sock 0600，B6）+ HTTP（axum 强制 Bearer，B5，端口 11435）并发。未配 `FUSION_MEMORY_API_KEY` 但 HTTP 端口开 → 拒启 HTTP（仅 UDS）。路由：`POST /v1/memory/{commit,retrieve,consolidate,audit,delete,delete_scope,count}`、`GET /v1/memory/{id}`、`GET /healthz`（公开）；`delete`/`delete_scope` 需 `params.confirm=true`。UDS method `memory.retrieve_context`（issue #1/#4 契约，`{trigger_id,query,top_k,node_id}` → `{context,memory_ids,cache_hit}`，复用 retrieve 引擎链）。

## 约定

- 4 空格缩进，无 docstring（`//!` 模块文档 + 行内注释）
- `tracing` 日志，`anyhow`（应用）+ `thiserror`（库）
- 失败可见，不静默吞错
