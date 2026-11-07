---
doc_id: fusion-memory/product-page
product: fusion-memory
class: core
entry: service
doc_type: product-page
title: { zh: "fusion-memory 产品页", en: "fusion-memory product page" }
audience: engineer
status: draft
version: 1.5.0
updated: 2026-11-07
owner: @dahai
source_of_truth:
  - README.md
  - CLAUDE.md
  - crates/fm-server/src/config.rs
---

# fusion-memory

```yaml
slug: memory
name: fusion-memory
tagline:
  zh: 系统级长期/短期记忆与认知图谱中枢：跨会话状态、三级调度、遗忘曲线 + 图谱对齐
  en: System-level long/short-term memory and cognitive-graph hub: cross-session state, three-tier scheduling, forgetting curve with KG alignment
cta:
  zh: 给 agent 挂上越用越懂你的记忆服务
  en: Give your agent a memory service that knows the user better with every use
repo: https://github.com/dahai80/fusion-memory
```

## 定位

```yaml
product:
  type: core-service
  serves: 需要跨会话记忆的 fusion 组件（fusion-agent-studio、fusion-cowork、fusion-code）
  builds_on:
    - fusion-mlx（:11434，bge-m3 嵌入 + 实体抽取；StubEmbedder 可全程离线）
    - fusion-store（向量页存储，本仓消费不重造）
    - 可选：fusion-identity（多租户 JWT 边界校验）
  hosts: 记忆提交/检索/合并、遗忘与巩固、知识图谱（SQLite 递归 CTE）、PII 脱敏、加密存储
  integration_surface: UDS JSON-RPC（0600）+ HTTP（:11435，Bearer）+ fm-py（PyO3 绑定）+ 内嵌 Web 控制台（/app）
  deployment: 本机或内网单实例；100% 离线（HTTP 仅 127.0.0.1，无云端调用）
```

## capabilities

```yaml
capabilities:
  - icon: 🧠
    name: { zh: 三级记忆调度, en: Three-tier memory scheduling }
    tag:
      zh: 工作记忆→短期→长期图谱自动流转，应用方不写升降级逻辑
      en: Working → short-term → long-term graph transitions run automatically; apps write no promotion logic
    points:
      - zh: commit 即抽取实体并入图谱；retrieve 组装压缩后上下文
        en: Commit extracts entities into the graph; retrieve assembles compressed context
      - zh: 约束：摘要/抽取质量依赖 fusion-mlx 模型（离线 stub 模式质量降级）
        en: Constraint: summary/extraction quality depends on fusion-mlx models; offline stub mode degrades quality
    viz: []
  - icon: 📉
    name: { zh: 遗忘曲线 + 巩固, en: Forgetting curve and consolidation }
    tag:
      zh: 衰减与巩固按算法跑夜间任务，不靠人工清理
      en: Decay and consolidation run algorithmically; no manual cleanup passes
    points:
      - zh: consolidate_memories 产出报告；fusion 评分经消融验证优于纯余弦（recall@5 +0.40，见 docs/benchmarks）
        en: consolidate_memories reports results; fusion scoring beats plain cosine by +0.40 recall@5 (see docs/benchmarks)
      - zh: 约束：scoped 巩固不推进全局水位，避免越权可见性
        en: Constraint: scoped consolidation never advances the global watermark, preserving visibility tiers
    viz: []
  - icon: 🔒
    name: { zh: 多租户与命名空间, en: Multi-tenancy and namespaces }
    tag:
      zh: 租户隔离 + user/project/local 可见性分层开箱即用
      en: Tenant isolation plus user/project/local visibility tiers out of the box
    points:
      - zh: multi_tenant 模式经 fusion-identity JWT 校验，tid 不匹配 403、空租户 401（fail-closed）
        en: multi_tenant mode verifies via fusion-identity JWT; tid mismatch 403, empty tenant 401 (fail-closed)
      - zh: 约束：网关源模式要求 X-Fusion-Route 头，直连调用会被拒
        en: Constraint: gateway-origin mode requires the X-Fusion-Route header; direct calls are rejected
    viz: []
  - icon: 🛡
    name: { zh: PII 脱敏 + 加密, en: PII redaction and encryption }
    tag:
      zh: 写入前五类 PII 正则脱敏，向量/图谱/检索全用脱敏后内容
      en: Five PII regex classes redacted before write; vectors, graph, and retrieval all see redacted content
    points:
      - zh: AES-256-GCM（key file 或 argon2id passphrase）；redaction fail-closed 默认开
        en: AES-256-GCM (key file or argon2id passphrase); redaction is fail-closed by default
      - zh: 约束：PII 处理在本仓本地，正式 DLP gate 等 fusion-guard 上游补齐
        en: Constraint: PII handling is local to this repo; the formal DLP gate waits on fusion-guard upstream
    viz: []
  - icon: 🖥
    name: { zh: Web 控制台 + 运维面, en: Web console and ops surface }
    tag:
      zh: 内嵌控制台浏览/审计/合并管理，不用手拼 RPC
      en: Embedded console for browse, audit, and merge management; no hand-built RPC
    points:
      - zh: /app 静态壳公开，数据全部走 Bearer /v1/*；list/audit_log/merges/unmerge/reconcile 五个管理 RPC
        en: /app static shell is public; all data flows through Bearer /v1/*; five admin RPCs (list/audit_log/merges/unmerge/reconcile)
      - zh: 约束：unmerge/reconcile 需 params.confirm=true，防误操作
        en: Constraint: unmerge/reconcile require params.confirm=true
    viz: []
```

## workflow

```yaml
workflow:
  - name: { zh: 构建/启动, en: Build and start }
    action: { zh: cargo build --release 或 ./start.sh start, en: cargo build --release or ./start.sh start }
    input: { zh: env 配置（API key、嵌入模型）, en: env config (API key, embed model) }
    output: { zh: UDS + :11435 /healthz 200, en: UDS plus :11435 /healthz 200 }
  - name: { zh: 提交记忆, en: Commit memories }
    action: { zh: POST /v1/memory/commit（会话交互片段）, en: POST /v1/memory/commit (session interactions) }
    input: { zh: session_id + interaction, en: session_id and interaction }
    output: { zh: 记忆 id + 图谱节点, en: memory id plus graph nodes }
  - name: { zh: 检索上下文, en: Retrieve context }
    action: { zh: /v1/memory/retrieve 或 UDS memory.retrieve_context, en: /v1/memory/retrieve or UDS memory.retrieve_context }
    input: { zh: query/top_k, en: query and top_k }
    output: { zh: 压缩后上下文 + 命中 id, en: compressed context plus hit ids }
  - name: { zh: 巩固与审计, en: Consolidate and audit }
    action: { zh: /v1/memory/consolidate 夜间跑；/app 或 audit_log 巡检, en: Run /v1/memory/consolidate nightly; inspect via /app or audit_log }
    input: { zh: confirm 参数（破坏性操作）, en: confirm params (destructive ops) }
    output: { zh: 巩固报告 + 审计记录, en: consolidation report plus audit trail }
```

## useCases

```yaml
useCases:
  - name: { zh: agent-studio 跨会话记忆, en: Cross-session memory in agent-studio }
    headline: { zh: 会话结束记忆不丢，下次开场即带上历史上下文, en: Memory survives sessions; the next one opens with context attached }
    body:
      zh: studio 经 UDS retrieve_context 组装开场上下文（#1/#4 契约）。差别：跨会话状态一份权威实现，消费方只调两个方法。
      en: The studio assembles opening context via UDS retrieve_context (#1/#4 contract). Difference: one authoritative cross-session implementation; consumers call two methods.
  - name: { zh: 多租户 SaaS 记忆隔离, en: Multi-tenant SaaS memory isolation }
    headline: { zh: 租户互不可见，身份边界在服务端强制, en: Tenants never see each other; the identity boundary is enforced server-side }
    body:
      zh: fm-server HTTP 边界校验 identity JWT，tid↔tenant 强制匹配。差别：应用不做租户判断，漏配直接 fail-closed。
      en: The fm-server HTTP boundary verifies identity JWTs and enforces tid↔tenant match. Difference: apps make no tenant decisions; misconfiguration fails closed.
```

## techDepth

```yaml
techDepth:
  - category: { zh: 可靠性纪律, en: Reliability disciplines }
    items:
      - name: { zh: 检索质量门禁, en: Retrieval-quality gate }
        desc: { zh: CI 门禁：fusion 永不低于纯余弦、时序新鲜优先、种子确定性, en: CI gates: fusion never below cosine, temporal freshness, seed determinism }
        steps:
          - zh: 见 crates/fm-engine/tests/retrieval_quality.rs, en: See crates/fm-engine/tests/retrieval_quality.rs
      - name: { zh: 注册表自检, en: Registry self-check }
        desc: { zh: start.sh 启动时校验 port-registry 注册项，未注册端口拒绝启动, en: start.sh verifies its port-registry entry at boot and refuses unregistered ports }
        steps:
          - zh: FUSION_SKIP_REGISTRY_CHECK=1 可旁路（告警）, en: FUSION_SKIP_REGISTRY_CHECK=1 bypasses with a warning
      - name: { zh: 环境变量契约, en: Env contract }
        desc: { zh: 全部 FUSION_MEMORY_* 有 README 清单与 env > TOML > secret-file > default 优先级, en: All FUSION_MEMORY_* vars are inventoried in README with env > TOML > secret-file > default priority }
        steps:
          - zh: 见 README Environment variables 一节, en: See the Environment variables section in README
```

## limits（已知边界）

- zh: 单实例（sled 独占锁）；graph 用 SQLite 递归 CTE 而非专用图库；100% 离线，无任何云端同步
- en: Single instance (sled exclusive lock); graph uses SQLite recursive CTE, not a dedicated graph DB; 100% offline, no cloud sync

## 提交检查清单

- [x] 所有字段 zh/en 成对
- [x] 每条能力回答"应用方少写了什么"
- [x] 端点/RPC/env 名与 README、fm-server/src/config.rs 一致
- [x] 数字有出处（recall@5 见 docs/benchmarks/retrieval-quality-2026-10-25.md）
- [x] 无营销形容词
