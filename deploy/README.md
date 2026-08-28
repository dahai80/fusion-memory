# Deploy Artifacts (P0-1 / P0-6)

进程守护 + 容器化部署制品。审计整改 P0-1 (无进程守护) + P0-6 (无部署制品)。

## 文件

| 文件 | 平台 | 用途 |
|------|------|------|
| `Dockerfile` | 容器 | 多阶段构建, 产物单镜像含 `fm-server` + `fm` CLI |
| `.dockerignore` | 容器 | 构建上下文瘦身 |
| `fusion-memory.service` | Linux (systemd) | 进程守护 unit, `Restart=always RestartSec=5s` |
| `io.fusion.memory.plist` | macOS (launchd) | LaunchAgent, `KeepAlive=true RunAtLoad=true` |
| `README.md` | — | 本文档 |

## P0-1 进程守护 (崩溃自动重启)

### Linux (systemd)

```bash
# 一键 (start.sh 自动拷 unit + 改路径 + enable):
sudo ./start.sh install

# 或手动:
sudo cp deploy/fusion-memory.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now fusion-memory
```

验收 (审计标准): `kill -9 $(pgrep fm-server)` 后 5s 内 `systemctl status fusion-memory` 重新 active。

```bash
sudo systemctl status fusion-memory        # 状态
journalctl -u fusion-memory -f             # 日志
sudo ./start.sh uninstall                  # 卸载
```

### macOS (launchd)

```bash
# 一键 (用户级 LaunchAgent, 无需 root):
./start.sh install

# 或手动:
cp deploy/io.fusion.memory.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/io.fusion.memory.plist
```

验收: `kill -9 $(pgrep fm-server)` 后 launchd 自动拉起 (`ThrottleInterval=5` 限 5s 间隔)。

```bash
tail -f ~/.fusion-memory/logs/launchd-stderr.log   # 日志
./start.sh uninstall                                # 卸载
```

### 配置

env 见 unit/plist 内 `Environment=` / `EnvironmentVariables`。必配项:

- `FM_HOME` — 数据目录 (fm 用户可写)
- `FUSION_MEMORY_API_KEY` — HTTP Bearer token (HTTP 端口开时必配, B5)
- `FUSION_MEMORY_STUB=1` — stub 离线模式 (默认); 真 bge-m3 需 fusion-mlx 对端

## P0-6 容器部署

### 构建

```bash
docker build -t fusion-memory:0.2.0 -f deploy/Dockerfile .
```

多阶段构建: builder (rust:1.87-bookworm) 编译 release 二进制并 strip; runtime (debian:bookworm-slim) 仅含二进制 + curl, 非 root (uid 1000), 镜像体积小。

### 运行 (stub 离线, 一键起)

```bash
docker run -d --name fm \
  -p 11435:11435 \
  -e FUSION_MEMORY_API_KEY=change-me \
  -e FUSION_MEMORY_STUB=1 \
  -v fm-data:/data \
  fusion-memory:0.2.0
```

### 验证

```bash
curl -sf http://127.0.0.1:11435/healthz    # 健康检查 (HEALTHCHECK 内建)
curl -sf http://127.0.0.1:11435/metrics    # Prometheus 指标 (P0-2)
```

### 真 bge-m3 模式

镜像默认 `FUSION_MEMORY_STUB=1`。真实 embedding 需 fusion-mlx 对端同网络可达:

```bash
docker run -d --name fm \
  -p 11435:11435 \
  -e FUSION_MEMORY_API_KEY=change-me \
  -e FUSION_MEMORY_STUB= \
  -e FUSION_MLX_URL=http://host.docker.internal:11434/v1 \
  -v fm-data:/data \
  fusion-memory:0.2.0
```

注: 离线约束 — fusion-mlx 须在宿主机或内网集群, 无外网。`host.docker.internal` 仅 dev; 生产用 `--network host` 或内网 DNS。

### 备份/恢复 (P0-4)

容器内 `fm` CLI 可用, 挂卷 `/data` 即 `FM_HOME`:

```bash
docker exec fm fm backup --dest /data/backups/manual   # 备份
docker exec fm fm restore --source /data/backups/manual --confirm   # 恢复 (需先停 server)
```
