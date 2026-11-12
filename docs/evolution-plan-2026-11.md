---
doc_id: fusion-memory/architecture/evolution-plan
product: fusion-memory
class: core
entry: service
doc_type: architecture
title: { zh: "能力补齐与超越计划 2026-11", en: "Catch-up and surpass plan 2026-11" }
audience: engineer
status: draft
version: 1.0.0
updated: 2026-11-12
owner: @dahai
source_of_truth:
  - docs/evolution-plan-2026-11.md
---

# 能力补齐与超越计划 2026-11

输入: `~/fusion/insight/fusion-memory-evolution-2026-11.md`(业界 memory 洞察,2026-11-12 实测数据)。
本文档把该洞察转成可执行的补齐 + 超越路线,绑定版本与验收门。执行纪律:每阶段一个 MINOR,
每项能力独立 issue + PR + 消融/复现证据,主线红线(§5)不可为赶工让步。

## 1. 差距清单(对照洞察 M1-M10)

| # | 差距 | 严重度 | 承载版本 |
|---|------|--------|----------|
| M1 | 检索单信号 — 无 BM25/FTS5 关键词路,纯语义+图 | 🔴 | v1.6.0 |
| M2 | 巩固非自动 — 仅 RPC/CLI 手动触发,无 dreaming 调度 | 🔴 | v1.6.0 |
| M4 | 无外部基准 — 无 LongMemEval/LoCoMo 接入 | 🟡 | v1.6.0 |
| M3 | 无事实有效期窗口 — 遗忘=删除而非时序排序 | 🟡 | v1.7.0 |
| M6 | 无记忆信任/投毒防护 — 抽取/巩固是注入面 | 🟡 | v1.7.0 |
| M5 | 无 per-memory ACL — 单 Bearer+tenant,无用户级授权 | 🟡 | v1.7.0 |
| M7 | procedural→SKILL.md 出口未接 | ⚪ 机会 | v1.8.0 |
| M8 | 无内容版本/标准导出 | 🟡 | v1.8.0 |
| M9 | 集群无自动 failover(选举 crate 在,未激活) | 🟡 | v1.9.0 |
| M10 | fm-py 无消费方 | 🟡 | v1.9.0 决断 |

## 2. 补齐路线(追平业界标配)

### 2.1 v1.6.0 — 检索与巩固(0-3 月,决定上限的两项)

1. **FTS5 关键词路(M1)**:memories 表加 FTS5 虚表(content,随写路径同步维护,复用 schema 迁移纪律升 SCHEMA_VERSION);检索改四路并行 — cosine / FTS5 BM25 / graph affinity / freshness,复用 α/β/γ 权重框架扩为四路,权重默认 0.4/0.3/0.2/0.1 起步。
   - 验收:`retrieval_quality.rs` 消融扩展 — 四路融合 ≥ 三路 ≥ 纯 cosine(recall@5 全量语料),CI 门禁沿用 never-below 原则;词法命中样例专测(专有名词/代号,语义路必然漏的 case)。
2. **dreaming 调度(M2)**:fm-server 内 tokio 定时器 + 空闲判定(无在飞 retrieve/commit 窗口),触发条件 = pending 增量阈值;等价"本地 sleep-time compute"。saga 四步内核零改动 — 只加触发器(洞察 §8.2 防线 4)。
   - 配置:`FUSION_MEMORY_DREAM_INTERVAL_SECS` / `FUSION_MEMORY_DREAM_PENDING_THRESHOLD`,默认关(向后兼容,开启即产品行为)。
   - 验收:调度单测(假时钟)+ 与在飞请求互斥测试 + consolidate_lock 复用证明;巩固边界审计日志(dreaming 触发/完成/失败全进 audit_log)。
3. **LongMemEval 子集进 CI(M4)**:本地跑通(embedder 可 stub 可 bge-m3 双模式),baseline JSON 入库;对外报数必附复现脚本(防线 6)。

### 2.2 v1.7.0 — 时间与信任(3-9 月)

4. **事实有效期(M3)**:MemoryItem 加 `valid_from`/`valid_until`(serde default,旧数据=无窗口);检索时序排序 — 过期降权不删除,tombstone 仍只给显式删除;与遗忘曲线相乘叠加(防线 3,改动带消融)。
5. **记忆信任链(M6)**:commit 抽取 + consolidate 蒸馏两处 LLM 输入过 fusion-security 注入检测(上游 issue 先行,同 fg-redact 模式);provenance 升级为 episodes 引用可查(对标 Graphiti 溯源);巩固产物带置信度字段(TRUSTMEM 传闻→置信度结论)。
6. **per-memory ACL(M5)**:namespace 之上加 user/role 授权位;身份源 = fusion-identity(已有 JWT 边界),tenant 仍只信 gateway 戳(防线 5);cowork/agent-studio 共用 11435 的多用户场景解锁。
7. **投毒治理面(M6 闭环)**:巩固是攻击面(MemoryDojo)— dreaming 的 merge/summarize 产物标记 provenance + 可 unmerge 撤销(已有机制),web UI 加溯源查询。

### 2.3 v1.8.0 — 技能合流与可携性(9-12 月)

8. **procedural→SKILL.md 出口(M7)**:巩固产物中 Procedural 记忆蒸馏为 SKILL.md 形态,导出给 agent-studio/fusion-harness skills 体系;对齐 Letta/MemOS/memU 的"记忆结晶成技能"合流;memU license 风险(NOASSERTION)下提供 clean Apache-2.0 替代路径。
9. **内容版本 + 导出(M8)**:修订历史表(内容 diff,audit_log 之外的版本层);标准导出 API — JSONL + Markdown(wikilink 图投影,对齐 Basic Memory/memU 文件式边界);导出路径强制过 redact 管道(防线 2)。

### 2.4 v1.9.0 — 规模化收尾

10. **集群自动 failover(M9)**:激活 fm-cluster 选举(v1.0.0 已有 lean election + v1.0.1 e2e),leader 失联自动晋升;跨节点同步验证。
11. **fm-py 决断(M10)**:fusion-trainer 训练数据侧接入则保留并补契约测试;否则标 deprecated 止损,文档给 HTTP 迁移路径。
12. **治理面板**:/app 升级 — 溯源查询/投毒审计/遗忘参数调优可视化。

## 3. 超越计划(差异化,业界没有的)

补齐追平后,凭 §4 底子在四个方向建立代差:

1. **数学化遗忘 × 有效期融合**:业界遗忘 = 时序排序(mem0/supermemory)或有效期窗口(Graphiti),没有产品同时有数学化遗忘曲线(W0/τ 分型 + 增益封顶)。v1.7.0 把两者做乘法叠加 — `score = fusion_score × freshness × validity_window`,三层时序语义一套消融框架量化。对外叙事:**可解释、可调参、有消融记录的遗忘**,而非黑盒排序。
2. **安全记忆全链路**:先脱敏后嵌入(已有)→ 注入检测(信任链)→ 巩固可撤销(saga+unmerge)→ 溯源可查(episodes)→ 字段加密 — 五层串联,业界无产品全覆盖;记忆投毒论文(MemoryDojo/TRUSTMEM)给出攻击分类,我们给防御实现并联合 fusion-security 出报告。
3. **巩固即治理**:把"dreaming"从产品词汇做成治理原语 — 调度触发/边界审计/失败追踪/逐条撤销/置信度标注,对齐 2611 安全文献的攻击面定义;这是从工程完整度到安全叙事的升维。
4. **离线单二进制 + 基准复现文化**:本地优先阵营(supermemory/MemOS)里唯一同时有 AES-GCM 静态加密 + gateway 信任链 + 消融入库 + LongMemEval CI 门禁的;对外每次报数附复现脚本,把"可验证"做成品牌。

## 4. 已守住的优势(不可回退项)

遗忘曲线数学(增益封顶 + raw W(t) 消融记录)/ 先脱敏后嵌入 / 100% 离线 + AES-256-GCM + gateway 信任链 / 巩固 saga 四步 + unmerge / 实测基准数字文化 + 515 测试 + SCHEMA_VERSION 迁移纪律。

## 5. 防线(执行期红线)

1. 新能力(调度/ACL/导出)不得引入默认外联 — 100% 离线是产品身份。
2. 一切新写入路径(skills 导出/导入/API 直写)过 redact 管道。
3. 遗忘曲线只叠加不推翻;任何打分改动带消融 + never-below CI 门禁。
4. saga 内核不动,自动化只加触发器;巩固产物全程可撤销可审计。
5. tenant 只信 gateway 戳,ACL 不开绕过网关的身份通道。
6. 基准数字必附复现脚本,防刷榜反噬。

## 6. 交付节点与验收总表

| 版本 | 主题 | 能力 | 验收门 |
|------|------|------|--------|
| v1.6.0 | 检索与巩固 | M1 四路融合 / M2 dreaming / M4 LongMemEval | 消融四路≥三路≥cosine;调度互斥单测;baseline JSON + 复现脚本 |
| v1.7.0 | 时间与信任 | M3 有效期 / M6 信任链 / M5 ACL | 叠加消融;注入检测 e2e(恶意样本);identity ACL 契约测试 |
| v1.8.0 | 技能与可携 | M7 SKILL.md / M8 版本+导出 | 导出过 redact 断言;agent-studio 消费 e2e;修订历史迁移 |
| v1.9.0 | 规模化 | M9 failover / M10 fm-py 决断 / 治理面板 | 3 节点自动晋升 e2e;fm-py keep-or-deprecate 决断记录 |

节奏风险对冲:每 MINOR 一个特性批次,单批超期不阻塞后续批次排队(版本号顺延,不合并批次);每批落地即走既有轮次流程(issue → 分支 → 全 gate → PR → tag → release)。
