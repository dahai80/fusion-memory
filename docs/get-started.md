---
doc_id: fusion-memory/get-started
product: fusion-memory
class: core
entry: service
doc_type: get-started
title: { zh: "fusion-memory 快速上手", en: "fusion-memory quick start" }
audience: engineer
status: draft
version: 1.5.0
updated: 2026-11-07
owner: "@dahai"
source_of_truth:
  - README.md
  - start.sh
  - CLAUDE.md
---

# fusion-memory 快速上手（Get Started）

> 目标：构建并启动记忆服务，完成一次提交与一次检索。

## 1. 前置条件

- Rust ≥ 1.87（MSRV；Homebrew rustc 1.96 可直接编译，无 rustup 依赖）
- 端口 `11435`（port-registry 注册；`FUSION_MEMORY_HTTP_PORT=0` 可关 HTTP 只用 UDS）
- 可选：fusion-mlx（:11434）跑真实 bge-m3 嵌入；不配则 `FUSION_MEMORY_STUB=1` 离线跑

## 2. 构建与启动

```bash
cargo build --release
./start.sh start            # 或直接运行 fm-server 二进制
```

确认点：

```bash
./start.sh status           # PID/port/sock/memory/healthz
./start.sh doctor           # binary/port/mlx connectivity/data dir
curl -s localhost:11435/healthz
```

- start.sh 启动时自检 port-registry 注册项，未注册端口拒绝启动（exit 1）
- 日志：`./start.sh log`

## 3. HTTP 必须带 key

`FUSION_MEMORY_API_KEY` 为空但 HTTP 端口打开时，服务拒绝启动 HTTP（仅 UDS）。设 key 后：

```bash
export FUSION_MEMORY_API_KEY="<your-key>"
curl -s localhost:11435/v1/memory/count -H "Authorization: Bearer $FUSION_MEMORY_API_KEY"
```

## 4. 第一次提交与检索

```bash
curl -s localhost:11435/v1/memory/commit \
  -H "Authorization: Bearer $FUSION_MEMORY_API_KEY" -H "Content-Type: application/json" \
  -d '{"session_id":"s1","interaction":{"user":"喜欢手冲咖啡","assistant":"记下了"}}'

curl -s localhost:11435/v1/memory/retrieve \
  -H "Authorization: Bearer $FUSION_MEMORY_API_KEY" -H "Content-Type: application/json" \
  -d '{"query":"咖啡偏好","top_k":5}'
```

确认点：commit 返回记忆 id；retrieve 返回组装后的上下文与命中 id 列表。

## 5. 跑测试与性能基线

```bash
cargo test --workspace                           # 496 offline tests（--features fm-store/store-fusion = 502）
cargo bench -p fm-engine --bench retrieve_bench  # store-stub 10k perf 基线（无模型）
```

## 6. 下一步 / 常见问题

全部 env 见 README「Environment variables」；端点全集见 [api-reference](api-reference.md)。

```yaml
troubleshooting:
  - q: { zh: "服务拒绝启动 HTTP", en: "Service refuses to start HTTP" }
    a: { zh: "HTTP 端口打开但 FUSION_MEMORY_API_KEY 为空会拒绝；设 key 或把端口设为 0 只用 UDS。", en: "An open HTTP port with an empty FUSION_MEMORY_API_KEY is refused; set a key or set the port to 0 for UDS-only." }
  - q: { zh: "start.sh 报端口未注册", en: "start.sh reports unregistered port" }
    a: { zh: "port-registry 自检失败；核对 FUSION_REGISTRY 指向的注册文件，勿随意旁路。", en: "The port-registry self-check failed; check the file FUSION_REGISTRY points at, do not bypass casually." }
  - q: { zh: "检索质量差", en: "Poor retrieval quality" }
    a: { zh: "确认在用真实 bge-m3（FUSION_MEMORY_STUB 未设）且 fusion-mlx 可达；stub 模式 FNV 嵌入仅供离线测试。", en: "Ensure real bge-m3 is in use (FUSION_MEMORY_STUB unset) and fusion-mlx is reachable; stub FNV embeddings are for offline tests only." }
```

## 提交检查清单

- [x] 构建/启动/提交/检索命令与 README、start.sh 一致
- [x] 每步有确认点
- [x] 端口 11435 在 port-registry 注册
- [x] zh/en 成对（命令除外）
- [x] 全篇 ≤ 120 行
