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

## v1.0.0 静态加密 (at-rest encryption)

分层策略 (纵深防御):

- **FDE (主)** — 全盘加密, ops 层。macOS FileVault / Linux LUKS (dm-crypt)。磁盘快照/物理失窃时整盘密文。
- **app 层 (纵深)** — SQLite `content` + `entities_json` 列 AES-256-GCM 加密。即使绕过 FDE 拿到 DB 文件也非明文。
- **向量** — 不 app 加密 (hnsw_rs 需明文算距离)。由 FDE + 上游 PII 脱敏 (fusion-guard) 覆盖。

### FDE 配置 (必配, 主静态加密)

```bash
# macOS: FileVault 全盘
sudo fdesetup enable   # 首次开启, 记录恢复密钥

# Linux: LUKS 加密 FM_HOME 所在分区
sudo cryptsetup luksFormat /dev/sdXN
sudo cryptsetup luksOpen /dev/sdXN fm-enc
sudo mkfs.ext4 /dev/mapper/fm-enc
sudo mount /dev/mapper/fm-enc /var/lib/fusion-memory
```

验收: `fdesetup status` (macOS) 显示 On / `cryptsetup status fm-enc` (Linux) 显示 active。

### app 层字段加密 (可选, 纵深防御)

两种 key 来源 (优先 file):

- `FUSION_MEMORY_ENC_KEY_FILE` — 0600 文件, 32B 原始 key
- `FUSION_MEMORY_ENC_PASSPHRASE` — argon2id KDF 派生 32B key

无 key = 明文模式 (向后兼容, 不加密)。

```bash
# 方式一: 32B 随机 key 文件 (推荐, 高熵)
head -c 32 /dev/urandom > /etc/fusion-memory/enc.key
chmod 0600 /etc/fusion-memory/enc.key
# systemd unit 加 Environment=FUSION_MEMORY_ENC_KEY_FILE=/etc/fusion-memory/enc.key
# launchd plist 加 <key>FUSION_MEMORY_ENC_KEY_FILE</key><string>/etc/fusion-memory/enc.key</string>

# 方式二: 口令 KDF (运维友好, 派生同 key)
# Environment=FUSION_MEMORY_ENC_PASSPHRASE=your-strong-passphrase
```

注:

- key 须稳定 — 换 key 后旧密文 fail-open 返原密文 (warn 留痕, 服务连续非 panic), 需重灌数据。
- 加密值带 `enc:v1:` 前缀, 读取端自动识别明文/密文混合。
- argon2id KDF 参数: 64MiB memory / 3 iterations / 4 lanes, 固定 salt (`fusion-memory-static-encryption-v1`)。

## P0-6 容器部署

### 构建

```bash
docker build -t fusion-memory:1.1.0-rc.1 -f deploy/Dockerfile .
```

多阶段构建: builder (rust:1.87-bookworm) 编译 release 二进制并 strip; runtime (debian:bookworm-slim) 仅含二进制 + curl, 非 root (uid 1000), 镜像体积小。

### 运行 (stub 离线, 一键起)

```bash
docker run -d --name fm \
  -p 11435:11435 \
  -e FUSION_MEMORY_API_KEY=change-me \
  -e FUSION_MEMORY_STUB=1 \
  -v fm-data:/data \
  fusion-memory:1.1.0-rc.1
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
  fusion-memory:1.1.0-rc.1
```

注: 离线约束 — fusion-mlx 须在宿主机或内网集群, 无外网。`host.docker.internal` 仅 dev; 生产用 `--network host` 或内网 DNS。

### 集群 + 自动 failover 选举 (v1.0.0 B-2)

集群 leader-follower 同步 (wop_log replay), leader 宕机 → follower 自动竞选新 leader (精简选举, 无 openraft 依赖)。选举需集群 ≥2 节点, 单机不启用。

必配 env (所有节点一致):

- `FUSION_MEMORY_ROLE` — `leader` / `follower` (当前角色; 自动 failover 胜出后 role file 自动改 leader, 重启生效)
- `FUSION_MEMORY_CLUSTER_NODES` — 全节点地址列表, 逗号分隔, 如 `127.0.0.1:11436,127.0.0.1:11437,127.0.0.1:11438`。索引即优先级 (小者高, 同 term 同日志新旧时低索引胜)
- `FUSION_MEMORY_CLUSTER_NODE_ID` — 自身节点下标 (0-based, 对应 NODES 列表位置)
- `FUSION_MEMORY_CLUSTER_TOKEN` — 集群共享 token, 投票/同步校验 (常时比较防时序攻击)

follower 还需:

- `FUSION_MEMORY_LEADER` — 当前 leader 地址 (leader 宕机后竞选胜出节点接管, 其余节点重启后改指新 leader)

可选调优 (复用 SyncConfig):

- `FUSION_MEMORY_HEARTBEAT_SECS` — 心跳间隔秒 (默认 5)。租约 = `heartbeat_secs × heartbeat_fails`
- `FUSION_MEMORY_HEARTBEAT_FAILS` — 连续失败阈值 (默认 3), 超过判 leader down → 触发竞选
- `FUSION_MEMORY_SYNC_PORT` — 同步端口 (默认 11436)
- `FUSION_MEMORY_CLUSTER_EPOCH` — 初始 epoch (fencing, 防 stale leader 写旧数据)

选举算法: leader-lease (心跳租约) + term 投票 + quorum (`floor(N/2)+1`) + 日志新旧 (`last_wop_seq`)。4 投票授予条件: term ≥ own、本 term 未投他人、候选日志 ≥ own、token 一致。胜出 → epoch++ + 写 role=Leader, 退出让 supervisor 重启成 leader。单机/手动模式不启用 (未配 NODES), `fm cluster promote` 手动 failover 仍保留。

验收 (3 节点示例, 同机不同端口):

```bash
# node0 (初始 leader):
FM_HOME=/data/n0 FUSION_MEMORY_ROLE=leader \
  FUSION_MEMORY_CLUSTER_NODES=127.0.0.1:11436,127.0.0.1:11437,127.0.0.1:11438 \
  FUSION_MEMORY_CLUSTER_NODE_ID=0 \
  FUSION_MEMORY_CLUSTER_TOKEN=secret ./target/release/fm-server &

# node1/node2 (follower, 同理改 NODE_ID + FM_HOME + 端口)
# kill node0 → node1 (低索引优先) 竞选胜出 → epoch++ → 自动成新 leader
fm cluster status    # 查 election state + epoch + nodes
```

注: 选举需所有节点都监听投票端口 (follower 也监听本节点地址供候选请求投票)。跨机部署设 `FUSION_MEMORY_CLUSTER_BIND_ADDR` 绑非 loopback。离线约束 — 仅内网, 无外网。

### 备份/恢复 (P0-4)

容器内 `fm` CLI 可用, 挂卷 `/data` 即 `FM_HOME`:

```bash
docker exec fm fm backup --dest /data/backups/manual   # 备份
docker exec fm fm restore --source /data/backups/manual --confirm   # 恢复 (需先停 server)
```
