---
doc_id: fusion-memory/config-reference
product: fusion-memory
class: core
entry: service
doc_type: config-reference
title: { zh: "fusion-memory 配置参考", en: "fusion-memory configuration reference" }
audience: engineer
status: draft
version: 1.5.0
updated: 2026-11-07
owner: "@dahai"
source_of_truth:
  - README.md
  - crates/fm-server/src/config.rs
---

# fusion-memory 配置参考（Configuration Reference）

配置来源优先级：**env > TOML（`FM_CONFIG`，默认 `<data_dir>/fusion-memory.toml`）> secret 文件 > 默认值**。全部变量 `FUSION_MEMORY_*`（存量 `FM_HOME`/`FM_CONFIG`/`FUSION_MLX_URL` 除外）。完整清单以 README「Environment variables」为权威。

## 服务与网络

| Env | 默认 | 说明 |
|---|---|---|
| `FM_HOME` | `~/.fusion-memory` | 数据目录，socket 路径由此派生 |
| `FM_CONFIG` | `<data_dir>/fusion-memory.toml` | TOML 配置路径 |
| `FUSION_MEMORY_SOCK` | `<data_dir>/fusion-memory.sock` | UDS socket 路径 |
| `FUSION_MEMORY_HTTP_PORT` | `11435` | HTTP 端口（0 = 关 HTTP） |
| `FUSION_MEMORY_API_KEY` / `_FILE` | 空 | HTTP Bearer key（空 = 拒开 HTTP）；`_FILE` 读 0600 文件 |
| `FUSION_MEMORY_UDS_TOKEN` / `_FILE` | 空 | UDS 连接级令牌（`AUTH <token>` 握手） |
| `FUSION_REGISTRY` | `~/fusion/architecture/port-registry.yaml` | start.sh 注册表自检源 |
| `FUSION_SKIP_REGISTRY_CHECK` | unset | `=1` 旁路自检（告警） |

## 租户与身份

| Env | 默认 | 说明 |
|---|---|---|
| `FUSION_MEMORY_MULTI_TENANT` | `false` | 开启 fusion-identity JWT 边界校验 |
| `FUSION_MEMORY_IDENTITY_URL` | `http://127.0.0.1:11470` | identity verify 端点 |
| `FUSION_MEMORY_IDENTITY_SERVICE_TOKEN` / `_FILE` | 空 | 校验调用用 service token |
| `FUSION_MEMORY_GATEWAY_ORIGIN_REQUIRED` | `false` | 要求 `X-Fusion-Route` 网关头 |
| `FUSION_MEMORY_DEFAULT_TENANT` | 空 | 调用方未带租户时的默认值 |
| `FUSION_MEMORY_ACL` | `false` | M5 #46 per-memory ACL（仅 `multi_tenant=true` 时生效；owner 位取 JWT `sub`，commit 权威覆盖，get/delete/list/retrieve/provenance 按 owner 过滤，admin 角色旁路；单开 ACL 无 identity 源 → 拒启） |

## 嵌入（fm-embed，env 与 `[embed]` TOML 同源，env 胜出）

| Env | 默认 | 说明 |
|---|---|---|
| `FUSION_MLX_URL` | `http://127.0.0.1:11434/v1` | fusion-mlx OpenAI 兼容基址 |
| `FUSION_MEMORY_EMBED_MODEL` | `bge-m3` | 嵌入模型名 |
| `FUSION_MEMORY_MLX_API_KEY` / `_FILE` | 空 | MLX API key |
| `FUSION_MEMORY_MLX_CONCURRENCY` | `2` | 并发嵌入请求数 |
| `FUSION_MEMORY_CHAT_MODEL` | `Qwen3.5-9B-4bit` | 实体抽取 chat 模型 |
| `FUSION_MEMORY_DIM` | `1024` | 嵌入维度（须 > 0） |
| `FUSION_MEMORY_STUB` | unset | `=1` 用 StubEmbedder（离线 FNV-1a） |

## 引擎与持久化

| Env | 默认 | 说明 |
|---|---|---|
| `FUSION_MEMORY_REDACT_PII` | 开（fail-closed） | `=0`/`=false` 关闭提交/导入前 PII 脱敏 |
| `FUSION_MEMORY_ENC_KEY_FILE` | unset | AES-256-GCM key 文件（0600，32B raw） |
| `FUSION_MEMORY_ENC_PASSPHRASE` | unset | argon2id KDF passphrase（二选一 key 源） |
| `FUSION_MEMORY_WOP_RETENTION` | 不裁剪 | 写操作日志保留上限（调用方侧裁剪） |
| `FUSION_MEMORY_INJECTION_URL` | unset | M6 #45 注入检测 fusion-security 委托端点（仅 127.0.0.1/localhost loopback，否则拒启）；未设 = 仅本地规则层 |
| `FUSION_MEMORY_INJECTION_TOKEN` | unset | 注入扫描 Bearer token（可选） |

## Dreaming 调度（fm-server，issue #40；默认关）

| 环境变量 | 默认 | 用途 |
|-----|---------|---------|
| `FUSION_MEMORY_DREAM_INTERVAL_SECS` | `0`（关） | 调度 tick 秒数；`0`/未设 = 完全禁用 dreaming 任务 |
| `FUSION_MEMORY_DREAM_PENDING_THRESHOLD` | `1` | 触发巩固的最小 pending 实体数 |

## 注意事项

- 开 HTTP 必须同时设 `FUSION_MEMORY_API_KEY`，否则服务拒绝启动 HTTP。
- `FUSION_MEMORY_STUB=1` 仅供离线测试，检索质量降级（见 [troubleshooting](troubleshooting.md)）。
- 端口 11435 已在 port-registry 注册；改端口需同步注册表。
