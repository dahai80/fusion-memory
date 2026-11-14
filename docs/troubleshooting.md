---
doc_id: fusion-memory/troubleshooting
product: fusion-memory
class: core
entry: service
doc_type: troubleshooting
title: { zh: "fusion-memory 故障排查", en: "fusion-memory troubleshooting" }
audience: engineer
status: draft
version: 1.5.0
updated: 2026-11-07
owner: "@dahai"
source_of_truth:
  - README.md
  - start.sh
---

# fusion-memory 故障排查（Troubleshooting）

先收集：`./start.sh status`（PID/port/sock）、`./start.sh doctor`（binary/port/mlx 连通/数据目录）、`curl -s localhost:11435/healthz`、`./start.sh log`。

## 症状速查

| 症状 | 原因 | 处置 |
|---|---|---|
| HTTP 拒绝启动 | HTTP 端口打开但 `FUSION_MEMORY_API_KEY` 为空 | 设 key，或 `FUSION_MEMORY_HTTP_PORT=0` 只用 UDS |
| start.sh exit 1（端口未注册） | port-registry 自检失败 | 核对 `FUSION_REGISTRY` 指向的注册文件是否含本服务条目；临时旁路 `FUSION_SKIP_REGISTRY_CHECK=1`（会告警） |
| sock 连接被拒 | 引擎未起或 socket 路径不符 | `./start.sh status` 看 sock 路径；确认客户端用同一 `FUSION_MEMORY_SOCK` |
| UDS 需要令牌 | 连接级鉴权开启 | 握手发 `AUTH <token>`（`FUSION_MEMORY_UDS_TOKEN`） |
| HTTP 401 | Bearer 缺失/错误；多租户模式空租户 | 带 `Authorization: Bearer`；多租户下必须携带含租户的身份 JWT |
| HTTP 403 | JWT tid 与请求租户不匹配 | 核对 fusion-identity 签发的 tid 与调用参数 |
| 检索结果质量差 | StubEmbedder 在用（FNV 嵌入）或 MLX 不可达 | 确认 `FUSION_MEMORY_STUB` 未设、`FUSION_MLX_URL` 可达、bge-m3 已加载 |
| embedding 报 401 Unauthorized（`invalid_api_key`） | #59：手管 key 文件与 fusion-mlx settings.json 漂移（key 轮换后文件失效） | 删除 `[embed].mlx_api_key_file` 指向的文件并取消配置 → 服务端回落读 `~/.fusion-mlx/settings.json` 的 `auth.api_key`（`FUSION_MEMORY_MLX_SETTINGS_JSON` 可覆盖路径）；`fm doctor` / `./start.sh doctor` 可交叉比对 |
| commit 报错维度不符 | `FUSION_MEMORY_DIM` 与实际嵌入维度不一致 | 保持 1024（bge-m3）或与模型一致 |
| delete/unmerge/reconcile 被拒 | 破坏性操作缺 confirm | 请求 params 加 `"confirm": true` |
| `-32602` | namespace 等参数非法（fail-closed） | 修正参数；合法可见性层级为 user/project/local |
| 导入失败：sled 独占锁 | import 与服务实例同时打开数据目录 | 走服务的 `POST /v1/memory/import`（共用同一引擎），勿另起第二实例 |
| 写入内容含明文 PII | `FUSION_MEMORY_REDACT_PII=0` 被显式关闭 | 恢复默认（fail-closed）；脱敏在 embed+persist 前执行，幂等 |
| CI 红在跨仓 path 依赖 | 本地 sibling 仓缺失 | v1.4.3 起已改 pinned git tag 依赖；确认在新 clone 中全量 `cargo test --workspace` |

## 排障开关

- `FUSION_MEMORY_STUB=1`：离线复现引擎行为（嵌入质量降级，仅测试用）。
- `FUSION_SKIP_REGISTRY_CHECK=1`：独立部署无注册表时旁路自检（告警）。

## 仍未解决

带 `./start.sh doctor` 输出 + 日志最后 100 行 + 复现请求提 issue（英文）。
