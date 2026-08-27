# fusion-memory 消费方参考客户端

M4 消费方接入参考实现。权威 PRD `architecture/fusion-memory-prd-0825.md` §10，落地架构 `~/fusion/fusion-memory-prd-plan-0826.md` §10。

本目录提供三个消费方各自接入 fusion-memory 的参考客户端 + 契约。**消费方在自己的仓库 vendor 对应文件**（跨工程改动遵循全局规则：上游 issue→PR）。

## 接入方式

| 消费方 | 协议 | 客户端 | 进程模型 |
|--------|------|--------|----------|
| fusion-agent-studio | PyO3 嵌入 (首选) / HTTP (备选) | `fm-py` crate → `import fusion_memory` / `python/fusion_memory_client.py` | 进程内直调省序列化 |
| fusion-cowork | PyO3 嵌入 (首选) / HTTP (备选) | `fm-py` crate / `python/fusion_memory_client.py` | 同上 |
| fusion-code | HTTP (无 PyO3) | `ts/fusionMemoryClient.ts` | 跨进程 HTTP 127.0.0.1 |

## PyO3 嵌入 (fm-py)

```python
import fusion_memory
engine = fusion_memory.Engine(data_dir="~/.fusion-memory")  # stub=False 用真 bge-m3
ids = engine.commit_episodic_memory(session_id, interaction_dict)
ctx = engine.retrieve_context(query_text, top_k=10, token_budget=2048)
report = engine.consolidate_memories()
```

构建 cdylib（macOS 需 `.cargo/config.toml` dynamic_lookup）：

```bash
PYO3_PYTHON=/opt/homebrew/bin/python3.12 cargo build -p fm-py
cp target/debug/libfusion_memory.dylib /path/to/fusion_memory.so
```

`allow_threads` 释 GIL，Rust 后台跑 mlx 不持 GIL，Python 事件循环不冻结（C2）。见 README.md「PyO3 往返验收」。

## HTTP (fm-server)

```bash
./start.sh start   # FUSION_MEMORY_API_KEY=<token> 必配 (B5)
```

wire 契约（JSON-RPC 2.0 envelope）：

```
POST /v1/memory/commit     {"jsonrpc":"2.0","method":"commit","params":{"session_id","interaction"},"id":1}
  → {"result":["<turn_id1>","<turn_id2>"]}
POST /v1/memory/retrieve   {"method":"retrieve","params":{"text","top_k","token_budget","aggregate"},"id":2}
  → {"result":{"blocks":[{interaction_id,turns,memory_type,turns_text,score,source_entities}],"total_tokens":N}}
POST /v1/memory/consolidate {"method":"consolidate","params":{},"id":3}
  → {"result":{"dropped","promoted","merged","summarized","reextracted","reconciled"}}
POST /v1/memory/audit      {"method":"audit","params":{"entity_ids":["..."]},"id":6}
  → {"result":[MemoryItem,...]}
POST /v1/memory/delete     {"method":"delete","params":{"id","confirm":true},"id":5}  # confirm 必填 (B5)
  → {"result":"deleted"}
GET  /v1/memory/{id}       → {"result":MemoryItem|null}
GET  /healthz             → "ok" (公开, 不鉴权)
```

所有 `/v1/*` 强制 `Authorization: Bearer <FUSION_MEMORY_API_KEY>`（B5）。未配 key 拒启 HTTP（仅 UDS）。UDS JSON-RPC 同语义，sock 0600（B6）。

## env (operator 配置)

| env | 默认 | 说明 |
|-----|------|------|
| `FUSION_MEMORY_HTTP_PORT` (fm-server) | 11435 | HTTP 端口 |
| `FUSION_MEMORY_API_KEY` | — | HTTP Bearer token，必配 |
| `FUSION_MEMORY_BASE_URL` (消费方) | 11435 (py) / 11440 (ts) | 消费方连的 URL |
| `FUSION_MEMORY_STUB` (fm-server) | 0 | 1=StubEmbedder 离线 |
| `FUSION_MEMORY_MLX_API_KEY` | — | mlx embedding/chat key |

### ⚠️ 端口冲突

fusion-code `fusion-kb-client.ts` 已占 11435。TS 客户端默认 11440 避让；operator 也可 `FUSION_MEMORY_HTTP_PORT=11440 ./start.sh start` + `FUSION_MEMORY_BASE_URL=http://127.0.0.1:11440` 消费方侧覆盖。Python 消费方默认 11435（与 fusion-memory PRD 一致，无冲突）。

## 各消费方接入缝 (recon 确认)

### fusion-agent-studio (§10.1, Python)

**后端替换、保留接口**。swap 点 `daemon_server.py:1891 _get_memory`，9 handler 签名不变。

| MemoryDispatcher handler | fusion-memory 映射 | 说明 |
|--------------------------|---------------------|------|
| `memory.store` | `commit` | content+metadata → Interaction 单 turn |
| `memory.recall` | `retrieve` | FTS5 query → semantic retrieve |
| `memory.list_recent` | `retrieve` (退化) | 空/通用 query + top_k 取近，消费方按时序排 |
| `memory.get` | `get` | 1:1 |
| `memory.delete` | `delete` (confirm) | 软删 |
| `memory.count` | 全量 `get` 累计 | 无 count RPC，消费方累计 |
| `memory.recall_relevant` | `retrieve` | 返回 context string |
| `memory.delete_scope` | **无映射** | 消费方适配层降级（按 scope 过滤后逐条 delete） |
| `memory.auto_forget` | `consolidate` | 遗忘 saga 远程等价 |

**三级映射**：`short_term`→Short / `long_term`→Long / `archive`→skip。`classify_memory_type` (user/feedback/project/reference) → MemoryType + 实体标签。历史 `memory.db` 一次性导入用 `fm-cli import`（M2，schema 已对齐 `import_studio.rs:32-46`）。

**不碰** `session_manager.py` / `persistence.py` / `compactor.py`（session 级，非长期记忆；compactor 仅经 `store_summary` 单点接触）。

### fusion-cowork (§10.3, Python DAG)

**新增 `memory` 节点类别（复用 `FUSION_ECOSYSTEM`，不加 `MEMORY` enum）+ 两节点**。仿 `trainer_node.py` 模式。

- `memory_commit`：把当前 workflow `TrajectoryRecorder` 输出 + `SharedContext` 作为 `Interaction` 调 `commit_episodic_memory`。
- `memory_retrieve`：输入 query → `retrieve_context` → 注入下游节点输入的 `_shared_context`。

接入缝：
1. 新文件 `nodes/ecosystem/memory_node.py` — 两 `@register_node` 类，`category=NodeCategory.FUSION_ECOSYSTEM`。
2. `nodes/__init__.py:13` `_NODE_MODULES` 追加 `"fusion_cowork.nodes.ecosystem.memory_node"`。
3. `nodes/ecosystem/__init__.py:3` re-export。
4. `__init__.py:19` `NODE_NAME_ALIASES` 加中文别名（"记忆提交"/"记忆检索"）。
5. HTTP 走 `httpx.AsyncClient`（pyproject 已有 `httpx>=0.27`，无新依赖）；或 PyO3 进程内直调。
6. `execute` 读 `inputs` dict（edge port routing 合并上游输出）+ `self.config.params`（`coerce_params`）；返 `NodeResult(data={...})`，data key 成下游输入。

**不碰** `SessionStore`（workflow session 状态，非长期记忆）。

### fusion-code (§10.2, TypeScript/Bun)

**HTTP 适配器 + 可选 UDS**。

- 新增 `src/services/memory/fusionMemoryClient.ts`（vendor `ts/fusionMemoryClient.ts`）。
- 注入缝：`AgentTool.tsx:836` — append `retrieve_context` 结果到 `enhancedSystemPrompt`（git context 后、chub hint 前）。
- commit 缝：`stopHooks.ts:153` — turn 结束 alongside `executeExtractMemories`，guard `!toolUseContext.agentId`（主线程 only）。
- HTTP 客户端风格参照 `fusion-kb-client.ts`（native `fetch` + `AbortSignal.timeout` + `logForDebugging` + 失败返空，不抛中断主流程）。
- 鉴权参照 `fusion-mlx-adapter.ts:289`（`Authorization: Bearer` + `X-Fusion-Route: local`）。
- 测试参照 `doctorMlxHealth.test.ts`（`spyOn(globalThis, "fetch")` + env snapshot/restore，`bun test`）。

**`memdir/MEMORY.md` 不删** — 人类可读本地镜像 + 离线兜底；fusion-memory 是语义检索增强层，不替代。同步方向单一：fusion-memory 只读 MEMORY.md（导入用），不回写（避免双写一致性）。

## 验收 (PRD §14 M4)

三消费方各跑通一个端到端跨 session 记忆召回场景：

- cowork：workflow memory_commit 落 trajectory → 新 session memory_retrieve 命中。
- fusion-code：session A turn 结束 commit → session B turn 开始 retrieve 命中注入。
- agent-studio：MemoryDispatcher.store 写偏好 → 新 session recall_relevant 命中。

契约场景测试见 `crates/fm-server/tests/consumer_scenarios.rs`（stub engine HTTP 往返，离线，证明 wire 契约）。
