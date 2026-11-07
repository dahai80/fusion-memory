---
doc_id: fusion-memory/api-reference
product: fusion-memory
class: core
entry: service
doc_type: api-reference
title: { zh: "fusion-memory API 参考", en: "fusion-memory API reference" }
audience: engineer
status: draft
version: 1.5.0
updated: 2026-11-07
owner: @dahai
source_of_truth:
  - README.md
  - crates/fm-server/src/
---

# fusion-memory API 参考（API Reference）

双通道并发：UDS JSON-RPC（socket 0600，连接级 `AUTH <token>` 握手可选）+ HTTP（axum，强制 Bearer，:11435）。HTTP 无 key 拒开（见 [troubleshooting](troubleshooting.md)）。`GET /healthz` 公开。

## HTTP 路由

| 路由 | confirm | 说明 |
|---|---|---|
| `GET /healthz` | — | 公开健康检查（无鉴权） |
| `GET /metrics` | — | Prometheus 指标 |
| `GET /v1/memory/version` | — | 服务版本 |
|---|---|---|
| `POST /v1/memory/commit` | — | 提交记忆片段，自动抽取实体入图谱 |
| `POST /v1/memory/retrieve` | — | 检索并组装压缩上下文 |
| `POST /v1/memory/consolidate` | — | 触发遗忘 + 巩固，返回报告 |
| `POST /v1/memory/audit` | — | 审计查询 |
| `GET /v1/memory/{id}` | — | 单条记忆 |
| `POST /v1/memory/delete` | ✔ | 删除（需 `params.confirm=true`） |
| `POST /v1/memory/delete_scope` | ✔ | 范围删除（需 confirm） |
| `POST /v1/memory/count` | — | 计数 |
| `POST /v1/memory/list` | — | 列表：`limit` 1..=200，`memory_type`/`namespace`/`include_tombstoned` 过滤；租户模式下内存级过滤墓碑 |
| `POST /v1/memory/audit_log` | — | 审计日志（limit 1..=500） |
| `POST /v1/memory/merges` | — | 合并记录查询 |
| `POST /v1/memory/unmerge` | ✔ | 拆分合并（需 confirm） |
| `POST /v1/memory/reconcile` | ✔ | 账目对账（需 confirm） |
| `POST /v1/memory/import` | — | HTTP-only 导入（body 即 params 对象 `{"source":...}`，默认 `~/.fusion-agent-studio/memory.db`，返回裸报告；与 CLI 共用同一引擎实例） |
| `GET /app` + `/app/*path` | 壳公开 | 内嵌 Web 控制台（数据全部走 Bearer /v1/*） |

## JSON-RPC 方法（UDS）

HTTP 路由与 UDS 方法同一分发层。UDS 契约方法（issue #1/#4）：

| 方法 | 参数 | 返回 |
|---|---|---|
| `memory.retrieve_context` | `{trigger_id, query, top_k, node_id}` | `{context, memory_ids, cache_hit}` |

其余方法与 HTTP 路由同名对应（`commit`/`retrieve`/`consolidate`/`delete`/...）。帧格式：`{"jsonrpc":"2.0","method":...,"params":...,"id":...}`；`import` 收裸 params 对象、回裸报告。

## fm-py（PyO3 绑定）

Python 侧经 `fm-py` cdylib 调用引擎（不占 sled 独占锁的第二连接不可用——绑定同样走服务或独立数据目录）。构建含 `fm-py` 时按 cargo 常规产出 cdylib；测试套件默认排除该 crate。

## 约定

- `namespace` 参数在 commit/retrieve/consolidate 可选；非法 namespace 返回 `-32602`（fail-closed）。
- 多租户模式：HTTP 边界先 `POST <identity_url>/api/v1/auth/verify` 验 JWT，tid↔tenant 不匹配 403、空租户 401。
- 破坏性操作（delete/delete_scope/unmerge/reconcile）无 confirm 直接拒绝。
