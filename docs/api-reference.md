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
owner: "@dahai"
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
| `POST /v1/memory/provenance` | — | M6 #45 溯源查询：`{id}` → `{id, sources, confidence, cited_by}`；他人 owner 记忆 → 404（ACL 开启时） |
| `POST /v1/memory/revisions` | — | M8 #49 修订历史：`{id}` → `[{rev, memory_id, content, reason, at}]`（旧值密文存储读取时解密）；主体不可见或他人记忆 → 404 |
| `POST /v1/memory/export` | — | M8 #49 标准导出：`{format: "jsonl"\|"markdown", limit 1..=1000, namespace?, memory_type?}` → `{format, count, body}`；渲染器内无条件 redact（防线 2）；排除 tombstone；ACL owner 过滤 |
| `POST /v1/memory/export_skills` | — | M7 #48 SKILL.md 导出：Procedural 记忆 → `[{id, name, confidence?, markdown}]`（YAML frontmatter 契约 + Provenance 尾注）；limit 1..=50；ACL owner 过滤 |
| `POST /v1/memory/config` | — | v1.9.0 #53 治理面板：只读返回评分/遗忘调优常量快照（融合权重/有效期因子/θ 阈值/图跳数） |
| `POST /v1/memory/revoke` | ✔ | v1.10.0 #57 按元数据撤销：`{filter: {scope?, authority?, persistence?, session_id?, namespace?}, confirm=true}` → `{revoked}`；空过滤 → 错误（fail-closed） |
| `POST /v1/memory/grant_access` | — | v1.10.0 #58 跨 agent 授权：`{grantor_key, grantee_key, namespace="agent"}`；仅 agent 命名空间，key 非空 ≤256 字符 |
| `POST /v1/memory/revoke_access` | ✔ | v1.10.0 #58 撤销授权：`{grantor_key, grantee_key, namespace?, confirm=true}` → `{revoked}` |
| `POST /v1/memory/list_grants` | — | v1.10.0 #58 授权查询：`{grantee_key, namespace?}` → `[{grantor_key, grantee_key, namespace, created_at}]` |
| `POST /v1/memory/publish_playbook` | — | v1.10.0 #58 发布到共享库：`{memory_id, publisher_key}` → `{playbook_id}`（playbook 命名空间 / team 权威 / pinned / 署名） |
| `POST /v1/memory/sleep_run` | — | v1.11.0 #60 睡眠治理：无参数，幂等触发一趟离线预计算 + 准入门重放 → `SleepReport`（consolidated/replay_checked/replay_downweighted/playbook_demoted/duration_ms） |
| `POST /v1/memory/list_namespaces` | — | v1.12.0 #64 命名空间 key 枚举：`{namespace: <tier>}` → 该租户该层的 distinct `namespace_key` 列表（只出 key 不出内容；空/未知层 invalid_params） |
| `POST /v1/memory/adopt_playbook` | — | v1.10.0 #58 采纳 playbook：`{playbook_id, consumer_namespace, consumer_key}` → `{memory_id}` 或 `{memory_id: null}`（准入门拒绝） |
| `GET /app` + `/app/*path` | 壳公开 | 内嵌 Web 控制台（数据全部走 Bearer /v1/*；v1.9.0 增 Governance 页 + Audit 页 trust 事件过滤） |

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
- M3 #44：`interaction.metadata` 可带 `valid_from`/`valid_until`（epoch ms）；过期记忆检索降权不删除。
- M5 #46：`FUSION_MEMORY_ACL=true`（须 multi_tenant）时 owner 位来自 JWT `sub`（服务端权威写入，客户端自报忽略）；不匹配的 get/delete/provenance/revisions 返回 404，retrieve/list/export 过滤；`role=admin` 旁路。
- M8 #49：所有导出路径（export/export_skills）在渲染器内无条件应用 redact 管道——`FUSION_MEMORY_REDACT_PII=0` 只覆盖摄入侧（commit/import），永不覆盖出口侧（防线 2）。
