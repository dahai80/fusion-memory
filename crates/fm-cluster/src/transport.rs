//! TCP 传输: Leader (服务 wop 拉取) + Follower (连 leader, fetch + heartbeat)。PRD §16。
//!
//! 单写点 leader: 写只走 leader persist, wop_log 追加。follower 读本地 (最终一致)。
//! 心跳: 5s TCP ping, 连续 3 失败 = leader down (M6 手动 failover, 不自动选举)。

use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};

use crate::config::SyncConfig;
use crate::error::{ClusterError, ClusterResult};
use crate::protocol::{
    read_frame, write_frame, Frame, FrameKind, Hello, SyncRequest, SyncResponse,
};
use crate::replay::{replay_wops, ReplaySink, WopSource};

/// leader: 监听 sync_port, 接 follower 连接, 响应 SyncRequest 返回 wop 增量。
pub struct Leader {
    source: Arc<dyn WopSource>,
    port: u16,
    /// §1.8: 绑定地址 (默认 127.0.0.1 loopback, 内网跨机设 0.0.0.0/内网 IP)。
    bind_addr: String,
    /// H3 鉴权: 配置则校验 follower hello.token, 不一致拒连接。None → §2.2 fail-closed
    /// (除非 allow_no_token=true, 仅单机测试)。
    cluster_token: Option<String>,
    /// §2.2: 无 token 显式放行 (仅 FUSION_MEMORY_CLUSTER_ALLOW_NO_TOKEN=1)。生产禁用。
    allow_no_token: bool,
    /// §1.8: leader epoch。自报给 follower, follower 校验 ≥ 期望值, 否则拒 (陈旧 leader fencing)。
    epoch: u64,
}

impl Leader {
    pub fn new(source: Arc<dyn WopSource>, port: u16) -> Self {
        Self {
            source,
            port,
            bind_addr: "127.0.0.1".to_string(),
            cluster_token: None,
            allow_no_token: false,
            epoch: 0,
        }
    }

    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.cluster_token = token;
        self
    }

    /// §2.2: 显式放行无 token (仅单机测试)。
    pub fn with_allow_no_token(mut self, allow: bool) -> Self {
        self.allow_no_token = allow;
        self
    }

    /// §1.8: 设置 leader epoch (failover 递增)。
    pub fn with_epoch(mut self, epoch: u64) -> Self {
        self.epoch = epoch;
        self
    }

    /// §1.8: 设置绑定地址 (内网跨机部署)。
    pub fn with_bind_addr(mut self, addr: impl Into<String>) -> Self {
        self.bind_addr = addr.into();
        self
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// 绑定监听端口, 返回 TcpListener + 实际端口 (0→OS 分配)。serve_listener 复用。
    /// §1.8: bind 用可配 bind_addr (非硬编码 127.0.0.1), 支持内网跨机部署。
    /// P1-6: 非 loopback 绑定 + 无 token → 拒启动 (防跨机明文 PII 外泄)。
    /// allow_no_token 不豁免非 loopback (该 flag 仅单机测试, 跨机必鉴权)。
    pub async fn bind(&self) -> ClusterResult<(TcpListener, u16)> {
        if !is_loopback(&self.bind_addr) && self.cluster_token.is_none() {
            error!(
                bind = %self.bind_addr,
                "cluster leader bind to non-loopback without token, refusing start (P1-6)"
            );
            return Err(ClusterError::BindRequiresToken {
                addr: self.bind_addr.clone(),
            });
        }
        let listener = TcpListener::bind(format!("{}:{}", self.bind_addr, self.port)).await?;
        let port = listener.local_addr()?.port();
        info!(port, bind = %self.bind_addr, "cluster leader bound");
        Ok((listener, port))
    }

    /// 启动 accept 循环 (cancel-safe: 每连接独立 task)。返回后阻塞, 调方需 spawn。
    pub async fn serve(self: Arc<Self>) -> ClusterResult<()> {
        let (listener, _) = self.bind().await?;
        info!(port = self.port, "cluster leader listening");
        self.serve_listener(listener).await
    }

    pub async fn serve_listener(self: Arc<Self>, listener: TcpListener) -> ClusterResult<()> {
        info!(port = self.port, "cluster leader listening");
        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    let leader = Arc::clone(&self);
                    tokio::spawn(async move {
                        if let Err(e) = leader.handle_conn(stream).await {
                            warn!(%addr, error = %e, "leader conn ended");
                        }
                    });
                }
                Err(e) => {
                    error!(error = %e, "leader accept failed");
                    return Err(e.into());
                }
            }
        }
    }

    async fn handle_conn(self: Arc<Self>, mut stream: TcpStream) -> ClusterResult<()> {
        // 1. hello 握手 + H3 token 鉴权
        let hello_frame = read_frame(&mut stream)
            .await?
            .ok_or(ClusterError::Transport("hello missing".into()))?;
        if hello_frame.kind != FrameKind::Hello {
            return Err(ClusterError::Transport(format!(
                "expected hello, got {:?}",
                hello_frame.kind
            )));
        }
        let hello: Hello = hello_frame.decode_payload()?;
        // §2.6 lag 可观测: leader 不再静默丢弃 hello.follower_last_seq。算 lag = leader_seq - follower_seq,
        // log 出来。lag 越界 → warn (死 follower 信号, 运维可介入防 §2.6 磁盘填满)。
        if let Ok(leader_seq) = self.source.last_wop_seq() {
            let lag = leader_seq - hello.follower_last_seq;
            if lag > 0 {
                if lag > 1000 {
                    warn!(
                        follower_last_seq = hello.follower_last_seq,
                        leader_seq = leader_seq,
                        lag,
                        "follower lag large (>1000), possible stuck/dead follower (§2.6)"
                    );
                } else {
                    info!(
                        follower_last_seq = hello.follower_last_seq,
                        leader_seq, lag, "follower lag"
                    );
                }
            }
        }
        // §2.2 fail-closed: 无 token 配置 → 拒所有连接, 除非显式 allow_no_token (仅单机测试)。
        // 旧版 None → 整个鉴权块跳过 = 默认无鉴权 (明文 PII 上线, 重放攻击面)。
        match (&self.cluster_token, self.allow_no_token) {
            (Some(expected), _) => {
                if !constant_time_eq(expected, &hello.token) {
                    warn!("leader: follower token mismatch, rejecting conn");
                    return Err(ClusterError::Transport("auth: token mismatch".into()));
                }
            }
            (None, false) => {
                warn!("leader: no cluster token configured, rejecting conn (set FUSION_MEMORY_CLUSTER_TOKEN or FUSION_MEMORY_CLUSTER_ALLOW_NO_TOKEN=1 for single-node test)");
                return Err(ClusterError::AuthNotConfigured);
            }
            (None, true) => {
                // 显式放行: 仅单机测试。生产禁用 (warn 提醒)。
                warn!("leader: no token but ALLOW_NO_TOKEN=1, accepting (single-node test only, NOT production)");
            }
        }
        // 2. 循环响应 SyncRequest / Ping
        while let Some(frame) = read_frame(&mut stream).await? {
            match frame.kind {
                FrameKind::SyncRequest => {
                    let req: SyncRequest = frame.decode_payload()?;
                    let entries = self.source.list_wop_since(req.since_seq, req.limit)?;
                    // §3.19: 去 leader_last_seq 死字段 (follower 用 outcome.last_applied_seq 推游标,
                    // 算它 = leader 热路径每请求额外 SQLite 查询浪费)。改载 leader_epoch (§1.8 fencing)。
                    let resp = SyncResponse {
                        entries,
                        leader_epoch: self.epoch,
                    };
                    write_frame(&mut stream, &Frame::new(FrameKind::SyncResponse, resp)?).await?;
                }
                FrameKind::Ping => {
                    write_frame(&mut stream, &Frame::new(FrameKind::Pong, "ok")?).await?;
                }
                other => {
                    warn!(kind = ?other, "leader unexpected frame, closing");
                    break;
                }
            }
        }
        Ok(())
    }
}

/// 常时比较 token (防 timing attack)。同长逐字节 XOR, 不短路。
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// P1-6: 判定 bind_addr 是否 loopback (127.0.0.1/::1/localhost)。非 loopback 绑定需鉴权。
/// 0.0.0.0 / 通配 / 内网 IP / 域名 → 视为非 loopback (保守, 域名解析前无法确认)。
fn is_loopback(addr: &str) -> bool {
    let a = addr.trim();
    a == "127.0.0.1"
        || a == "::1"
        || a.eq_ignore_ascii_case("localhost")
        || a == "[::1]"
        || a.starts_with("127.")
}

/// follower: 连 leader, 周期 fetch wop 增量本地重放 + 心跳。PRD §16.5。
pub struct Follower {
    cfg: SyncConfig,
    sink: Arc<dyn ReplaySink>,
    local_last_seq: i64,
}

impl Follower {
    pub fn new(cfg: SyncConfig, sink: Arc<dyn ReplaySink>, local_last_seq: i64) -> Self {
        Self {
            cfg,
            sink,
            local_last_seq,
        }
    }

    pub fn local_last_seq(&self) -> i64 {
        self.local_last_seq
    }

    /// 单次同步: 连 leader → hello(token+epoch) → sync request → 重放。
    /// 返回 (新 last_seq, 应用数, 本批是否含落地失败条目, 是否永久失败)。
    /// H2 修正: local_last_seq 推进到本批已落地条目的最大 seq (last_applied_seq),
    /// 而非 leader_last_seq。失败条目下轮重拉, 已落地条目不重复重放 (stub 幂等)。
    /// §1.8: 校验 resp.leader_epoch ≥ 期望 epoch, 否则 StaleLeader 永久错误 (防脑裂双写)。
    /// 传输层失败 (connect/帧坏/decode) 仍返 Err → caller 判 leader 宕机倾向。
    /// 单条落地失败 → Ok(_, _, true, permanent), 不抛 Err → caller 不触发 failover (非 leader 宕机)。
    pub async fn sync_once(&mut self) -> ClusterResult<(i64, usize, bool, bool)> {
        let mut stream = TcpStream::connect(&self.cfg.leader_addr).await?;
        let hello = Hello {
            follower_last_seq: self.local_last_seq,
            token: self.cfg.cluster_token.clone().unwrap_or_default(),
            epoch: self.cfg.epoch,
        };
        write_frame(&mut stream, &Frame::new(FrameKind::Hello, hello)?).await?;
        let req = SyncRequest {
            since_seq: self.local_last_seq,
            limit: self.cfg.fetch_limit,
        };
        write_frame(&mut stream, &Frame::new(FrameKind::SyncRequest, req)?).await?;
        let resp_frame = read_frame(&mut stream)
            .await?
            .ok_or(ClusterError::Transport("sync response missing".into()))?;
        if resp_frame.kind != FrameKind::SyncResponse {
            return Err(ClusterError::Transport(format!(
                "expected sync response, got {:?}",
                resp_frame.kind
            )));
        }
        let resp: SyncResponse = resp_frame.decode_payload()?;
        // §1.8 fencing: follower 期望 epoch > 0 时, leader_epoch < 期望 → 陈旧 leader, 拒同步防脑裂。
        // (分区后旧 leader epoch 未递增, 新 leader promote 后 epoch+1; follower 连旧 leader 被拒)
        if self.cfg.epoch > 0 && resp.leader_epoch < self.cfg.epoch {
            warn!(
                leader_epoch = resp.leader_epoch,
                expected = self.cfg.epoch,
                "stale leader detected, rejecting sync (fencing)"
            );
            return Err(ClusterError::StaleLeader {
                leader_epoch: resp.leader_epoch,
                expected_epoch: self.cfg.epoch,
            });
        }
        let outcome = replay_wops(self.sink.as_ref(), &resp.entries).await;
        // H2: 推进到 last_applied_seq (已落地/跳过最大 seq), 非 leader_last_seq。
        // 失败条目不计入 last_applied_seq → 游标卡在失败条目前, 下轮重拉。
        if outcome.last_applied_seq > self.local_last_seq {
            self.local_last_seq = outcome.last_applied_seq;
        }
        Ok((
            self.local_last_seq,
            outcome.applied,
            outcome.failed,
            outcome.permanent,
        ))
    }

    /// 心跳: 连 leader 发 ping, 期待 pong。成功 true。
    pub async fn heartbeat(&self) -> ClusterResult<bool> {
        let mut stream = TcpStream::connect(&self.cfg.leader_addr).await?;
        write_frame(
            &mut stream,
            &Frame::new(
                FrameKind::Hello,
                Hello {
                    follower_last_seq: self.local_last_seq,
                    token: self.cfg.cluster_token.clone().unwrap_or_default(),
                    epoch: self.cfg.epoch,
                },
            )?,
        )
        .await?;
        write_frame(&mut stream, &Frame::new(FrameKind::Ping, "hb")?).await?;
        let pong = read_frame(&mut stream)
            .await?
            .ok_or(ClusterError::Transport("heartbeat no pong".into()))?;
        Ok(pong.kind == FrameKind::Pong)
    }

    /// 持续同步循环: fetch → sleep → repeat。心跳 N 失败 → LeaderDown。
    /// H2 修正: 区分 transport-fail (连不上 leader, 真宕机 → 计 fails → LeaderDown)
    /// 与 replay-fail (连上但单条落地失败, 与 leader 存活无关 → 不计 fails, 退避重试, 不触发 failover)。
    /// §3.19: 退避加 jitter (基于 follower 本地 wop seq, 确定性, 避 5 follower 雷鸣群击中 leader 恢复)。
    /// §3.19: 永久重放失败 (payload 损坏/陈旧 epoch) → 不退避重试, error 升级, 游标卡住等运维。
    pub async fn run(mut self) -> ClusterResult<()> {
        info!(leader = %self.cfg.leader_addr, "follower sync loop start");
        let mut fails: u32 = 0;
        let mut replay_backoff: u64 = 1;
        loop {
            match self.sync_once().await {
                Ok((_seq, applied, replay_failed, permanent)) => {
                    if applied > 0 {
                        info!(applied, "follower synced");
                    }
                    fails = 0;
                    if replay_failed {
                        if permanent {
                            // §3.19: 永久失败 (payload 损坏/陈旧 epoch) — 重试无意义, 游标卡住。
                            // 不退避重试 (避免满盘 seq 永远卡在那 warn 重试)。error 升级, 等运维介入。
                            // 仍 sleep 正常间隔 (非 hot-spin), 下轮 sync_once 重新评估 (运维修数据后可恢复)。
                            // §2.5: 游标卡住 = follower 数据落后 leader 不前 → 标 stale_read (客户端可见)。
                            error!(
                                "follower replay PERMANENT fail, cursor stuck at seq {}, needs operator intervention",
                                self.local_last_seq
                            );
                            let _ = self.sink.on_sync_stale().await;
                            sleep(Duration::from_secs(self.cfg.heartbeat_secs)).await;
                            continue;
                        }
                        // 连上 leader 但本批有单条瞬时落地失败 → 非 leader 宕机, 退避重试, 不触发 failover。
                        // §3.19: 加 jitter (基于本地 seq, 确定性, 无 rand 依赖) — 退避 = base ± base/4,
                        // 各 follower seq 不同 → 退避散开, leader 恢复时不雷鸣群击中。
                        // §2.5: 瞬时单条失败不标 stale — 仅一条 wop 卡住, 其余增量已追平, 数据大体新鲜。
                        warn!("follower replay transient fail (leader alive), backoff retry");
                        replay_backoff = (replay_backoff * 2).min(60);
                        let jitter = backoff_jitter(replay_backoff, self.local_last_seq);
                        sleep(Duration::from_secs(replay_backoff + jitter)).await;
                        continue;
                    }
                    // §2.5: 本批干净落地/跳过, 与 leader 追平 → 通知 sink 清 stale_read + 记同步时间。
                    let _ = self.sink.on_sync_ok().await;
                    replay_backoff = 1;
                }
                Err(e) => {
                    // §1.8: StaleLeader/AuthNotConfigured = 永久, 但非 leader 宕机 → 不计 fails,
                    // 不退避重试 (重连同陈旧 leader 无意义), error 升级, 等运维 promote 新 leader。
                    if e.is_permanent() {
                        error!(error = %e, "follower sync PERMANENT error, not retrying (needs operator: promote new leader or fix config)");
                        // §2.5: 永久错误 → follower 数据停滞 → 标 stale_read。
                        let _ = self.sink.on_sync_stale().await;
                        sleep(Duration::from_secs(self.cfg.heartbeat_secs)).await;
                        continue;
                    }
                    // transport/io/serde → 连不上或帧坏, 真宕机倾向 → 计 fails。
                    fails += 1;
                    warn!(fails, error = %e, "follower sync fail (transport)");
                    // §2.5: 连续 transport 失败 = follower 落后 leader 加深 → 标 stale_read (从首次失败起)。
                    let _ = self.sink.on_sync_stale().await;
                    if fails >= self.cfg.heartbeat_fails {
                        return Err(ClusterError::LeaderDown(fails));
                    }
                }
            }
            sleep(Duration::from_secs(self.cfg.heartbeat_secs)).await;
        }
    }
}

/// §3.19: 确定性退避 jitter (无 rand 依赖)。基于 base 与 seed (follower 本地 seq) 算 0..=base/4 偏移。
/// 各 follower seed 不同 → 退避散开, leader 恢复时避免雷鸣群击中。base=1 时 jitter=0 (首轮无抖动必要)。
fn backoff_jitter(base: u64, seed: i64) -> u64 {
    if base <= 1 {
        return 0;
    }
    let span = base / 4;
    // seed 可能负 (空库 seq=0), 取绝对值 + 1 防零。
    let s = seed.unsigned_abs().wrapping_add(1);
    // 简单确定性散列: 黄金比例乘法散列 (Knuth), 落 [0, span]。
    let h = s.wrapping_mul(11400714819323198485);
    if span == 0 {
        0
    } else {
        h % (span + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::WopSource;
    use fm_persist::WopEntry;
    use std::sync::Mutex;

    struct StubSource {
        entries: Mutex<Vec<WopEntry>>,
    }

    impl WopSource for StubSource {
        fn list_wop_since(&self, since_seq: i64, limit: usize) -> ClusterResult<Vec<WopEntry>> {
            let all = self.entries.lock().unwrap();
            let out: Vec<WopEntry> = all
                .iter()
                .filter(|e| e.seq > since_seq)
                .take(limit)
                .cloned()
                .collect();
            Ok(out)
        }
        fn last_wop_seq(&self) -> ClusterResult<i64> {
            Ok(self
                .entries
                .lock()
                .unwrap()
                .iter()
                .map(|e| e.seq)
                .max()
                .unwrap_or(0))
        }
    }

    #[tokio::test]
    async fn leader_serves_sync_request() {
        let source = Arc::new(StubSource {
            entries: Mutex::new(vec![WopEntry {
                seq: 1,
                op: "delete".into(),
                payload: "x".into(),
                at: 100,
            }]),
        });
        let leader = Arc::new(Leader::new(source, 0).with_allow_no_token(true));
        let (listener, port) = leader.bind().await.unwrap();
        let leader_task = tokio::spawn(Arc::clone(&leader).serve_listener(listener));
        // sink 计数 delete
        let sink = Arc::new(CountingSink::new());
        let mut follower = Follower::new(
            SyncConfig {
                leader_addr: format!("127.0.0.1:{port}"),
                heartbeat_secs: 1,
                heartbeat_fails: 3,
                fetch_limit: 64,
                cluster_token: None,
                epoch: 0,
            },
            sink.clone(),
            0,
        );
        let (seq, applied, failed, _perm) = follower.sync_once().await.unwrap();
        assert_eq!(seq, 1);
        assert_eq!(applied, 1);
        assert!(!failed);
        assert_eq!(sink.tombstones(), vec!["x".to_string()]);
        leader_task.abort();
    }

    #[tokio::test]
    async fn follower_heartbeat_ok() {
        let source = Arc::new(StubSource {
            entries: Mutex::new(vec![]),
        });
        let leader = Arc::new(Leader::new(source, 0).with_allow_no_token(true));
        let (listener, port) = leader.bind().await.unwrap();
        let leader_task = tokio::spawn(Arc::clone(&leader).serve_listener(listener));
        let sink = Arc::new(CountingSink::new());
        let follower = Follower::new(
            SyncConfig {
                leader_addr: format!("127.0.0.1:{port}"),
                heartbeat_secs: 1,
                heartbeat_fails: 3,
                fetch_limit: 64,
                cluster_token: None,
                epoch: 0,
            },
            sink,
            0,
        );
        assert!(follower.heartbeat().await.unwrap());
        leader_task.abort();
    }

    #[tokio::test]
    async fn follower_sync_leader_down_refused() {
        let sink = Arc::new(CountingSink::new());
        let mut follower = Follower::new(
            SyncConfig {
                leader_addr: "127.0.0.1:1".into(),
                heartbeat_secs: 0,
                heartbeat_fails: 2,
                fetch_limit: 64,
                cluster_token: None,
                epoch: 0,
            },
            sink,
            0,
        );
        let err = follower.sync_once().await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn leader_port_and_follower_local_seq_accessors() {
        // 覆盖 port()/local_last_seq() 访问器。
        let source = Arc::new(StubSource {
            entries: Mutex::new(vec![]),
        });
        let leader = Leader::new(source, 0);
        assert_eq!(leader.port(), 0);
        let sink = Arc::new(CountingSink::new());
        let follower = Follower::new(
            SyncConfig {
                leader_addr: "127.0.0.1:1".into(),
                heartbeat_secs: 1,
                heartbeat_fails: 3,
                fetch_limit: 64,
                cluster_token: None,
                epoch: 0,
            },
            sink,
            42,
        );
        assert_eq!(follower.local_last_seq(), 42);
    }

    #[tokio::test]
    async fn leader_serve_binds_and_accepts() {
        // 覆盖 serve() (bind + serve_listener 一体路径), 非显式 serve_listener。
        // 先探一个空闲端口, drop 后交给 serve() bind (微秒级窗口, 测试环境可接受)。
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let source = Arc::new(StubSource {
            entries: Mutex::new(vec![]),
        });
        let leader = Arc::new(Leader::new(source, port).with_allow_no_token(true));
        let task = tokio::spawn(Arc::clone(&leader).serve());
        // 等 serve() 内部 bind 完成 (避免 follower connect 撞未就绪端口)。
        for _ in 0..20 {
            if tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let sink = Arc::new(CountingSink::new());
        let mut follower = Follower::new(
            SyncConfig {
                leader_addr: format!("127.0.0.1:{port}"),
                heartbeat_secs: 1,
                heartbeat_fails: 3,
                fetch_limit: 64,
                cluster_token: None,
                epoch: 0,
            },
            sink,
            0,
        );
        follower.sync_once().await.unwrap();
        task.abort();
    }

    #[tokio::test]
    async fn leader_handle_conn_rejects_non_hello_first_frame() {
        // 覆盖 handle_conn: 第一帧非 Hello → 返回错误 (连接关闭)。
        use crate::protocol::{write_frame, Frame, FrameKind};
        let source = Arc::new(StubSource {
            entries: Mutex::new(vec![]),
        });
        let leader = Arc::new(Leader::new(source, 0).with_allow_no_token(true));
        let (listener, port) = leader.bind().await.unwrap();
        let task = tokio::spawn(Arc::clone(&leader).serve_listener(listener));
        let mut s = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        // 发 Ping 当首帧 (非 Hello) → leader 关连接
        write_frame(&mut s, &Frame::new(FrameKind::Ping, "x").unwrap())
            .await
            .unwrap();
        // 再写应失败/EOF (leader 已关)
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 16];
        let _ = s.read(&mut buf).await;
        task.abort();
    }

    #[tokio::test]
    async fn follower_sync_once_rejects_non_sync_response() {
        // 覆盖 sync_once: leader 回非 SyncResponse → 错误。
        use crate::protocol::{write_frame, Frame, FrameKind};
        // 假 leader: 回 Pong 而非 SyncResponse
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let fake_task = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let _ = read_frame(&mut s).await; // hello
            let req = read_frame(&mut s).await; // sync request
            let _ = req;
            // 回 Pong (错误类型)
            write_frame(&mut s, &Frame::new(FrameKind::Pong, "wrong").unwrap())
                .await
                .unwrap();
        });
        let sink = Arc::new(CountingSink::new());
        let mut follower = Follower::new(
            SyncConfig {
                leader_addr: format!("127.0.0.1:{port}"),
                heartbeat_secs: 1,
                heartbeat_fails: 3,
                fetch_limit: 64,
                cluster_token: None,
                epoch: 0,
            },
            sink,
            0,
        );
        let err = follower.sync_once().await;
        assert!(err.is_err());
        fake_task.abort();
    }

    #[tokio::test]
    async fn leader_rejects_token_mismatch() {
        // H3: leader 配 token, follower 带错 token → leader 拒连接, sync_once 报 transport err。
        let source = Arc::new(StubSource {
            entries: Mutex::new(vec![]),
        });
        let leader = Arc::new(Leader::new(source, 0).with_token(Some("secret-token".into())));
        let (listener, port) = leader.bind().await.unwrap();
        let leader_task = tokio::spawn(Arc::clone(&leader).serve_listener(listener));
        let sink = Arc::new(CountingSink::new());
        let mut follower = Follower::new(
            SyncConfig {
                leader_addr: format!("127.0.0.1:{port}"),
                heartbeat_secs: 1,
                heartbeat_fails: 3,
                fetch_limit: 64,
                cluster_token: Some("wrong-token".into()),
                epoch: 0,
            },
            sink,
            0,
        );
        let err = follower.sync_once().await;
        assert!(err.is_err(), "token mismatch must reject");
        leader_task.abort();
    }

    #[tokio::test]
    async fn leader_accepts_matching_token() {
        // H3: leader + follower 同 token → sync_once 成功。
        let source = Arc::new(StubSource {
            entries: Mutex::new(vec![]),
        });
        let leader = Arc::new(Leader::new(source, 0).with_token(Some("shared-secret".into())));
        let (listener, port) = leader.bind().await.unwrap();
        let leader_task = tokio::spawn(Arc::clone(&leader).serve_listener(listener));
        let sink = Arc::new(CountingSink::new());
        let mut follower = Follower::new(
            SyncConfig {
                leader_addr: format!("127.0.0.1:{port}"),
                heartbeat_secs: 1,
                heartbeat_fails: 3,
                fetch_limit: 64,
                cluster_token: Some("shared-secret".into()),
                epoch: 0,
            },
            sink,
            0,
        );
        let (seq, _applied, failed, _perm) = follower.sync_once().await.unwrap();
        assert_eq!(seq, 0, "empty source, no advance");
        assert!(!failed);
        leader_task.abort();
    }

    #[tokio::test]
    async fn leader_rejects_no_token_fail_closed() {
        // §2.2: leader 无 token + 未显式 allow_no_token → 拒所有连接 (fail-closed)。
        // 旧版 None → 鉴权块跳过 = 默认无鉴权 (明文 PII 上线)。现返 AuthNotConfigured。
        let source = Arc::new(StubSource {
            entries: Mutex::new(vec![]),
        });
        let leader = Arc::new(Leader::new(source, 0)); // 无 token, 无 allow_no_token
        let (listener, port) = leader.bind().await.unwrap();
        let leader_task = tokio::spawn(Arc::clone(&leader).serve_listener(listener));
        let sink = Arc::new(CountingSink::new());
        let mut follower = Follower::new(
            SyncConfig {
                leader_addr: format!("127.0.0.1:{port}"),
                heartbeat_secs: 1,
                heartbeat_fails: 3,
                fetch_limit: 64,
                cluster_token: None,
                epoch: 0,
            },
            sink,
            0,
        );
        let err = follower.sync_once().await;
        assert!(err.is_err(), "no-token leader must fail-closed reject");
        // leader 端返 AuthNotConfigured 后关连接, follower 跨 TCP 只见 ConnectionReset/EOF
        // (无法区分 auth-reject vs leader crash — 运行时正确: 连接被拒 = 同步失败)。
        // 关键断言: 无 token leader 不再静默放行 (旧版会成功同步 = 明文 PII 上线)。
        let ok = matches!(
            err,
            Err(ClusterError::AuthNotConfigured)
                | Err(ClusterError::Io(_))
                | Err(ClusterError::Transport(_))
        );
        assert!(
            ok,
            "expected auth-reject surfaced as conn-fail, got {err:?}"
        );
        leader_task.abort();
    }

    #[tokio::test]
    async fn follower_rejects_stale_leader_epoch() {
        // §1.8 fencing: follower 期望 epoch=2, leader 自报 epoch=1 (陈旧) → StaleLeader 拒同步。
        // 场景: 分区后旧 leader epoch 未递增, 新 leader promote 后 epoch+1; follower 连旧 leader 被拒。
        let source = Arc::new(StubSource {
            entries: Mutex::new(vec![]),
        });
        // 旧 leader: epoch=1, 配 token, allow_no_token=false
        let leader = Arc::new(
            Leader::new(source, 0)
                .with_token(Some("shared".into()))
                .with_epoch(1),
        );
        let (listener, port) = leader.bind().await.unwrap();
        let leader_task = tokio::spawn(Arc::clone(&leader).serve_listener(listener));
        let sink = Arc::new(CountingSink::new());
        let mut follower = Follower::new(
            SyncConfig {
                leader_addr: format!("127.0.0.1:{port}"),
                heartbeat_secs: 1,
                heartbeat_fails: 3,
                fetch_limit: 64,
                cluster_token: Some("shared".into()),
                epoch: 2, // follower 期望 epoch=2 > leader epoch=1
            },
            sink,
            0,
        );
        let err = follower.sync_once().await;
        assert!(err.is_err(), "stale leader must be fenced");
        assert!(
            matches!(
                err,
                Err(ClusterError::StaleLeader {
                    leader_epoch: 1,
                    expected_epoch: 2
                })
            ),
            "expected StaleLeader{{1,2}}, got {:?}",
            err
        );
        leader_task.abort();
    }

    #[tokio::test]
    async fn follower_accepts_equal_or_higher_leader_epoch() {
        // §1.8: leader epoch=3 ≥ follower 期望 epoch=2 → 正常同步 (无 fencing)。
        let source = Arc::new(StubSource {
            entries: Mutex::new(vec![]),
        });
        let leader = Arc::new(
            Leader::new(source, 0)
                .with_token(Some("shared".into()))
                .with_epoch(3),
        );
        let (listener, port) = leader.bind().await.unwrap();
        let leader_task = tokio::spawn(Arc::clone(&leader).serve_listener(listener));
        let sink = Arc::new(CountingSink::new());
        let mut follower = Follower::new(
            SyncConfig {
                leader_addr: format!("127.0.0.1:{port}"),
                heartbeat_secs: 1,
                heartbeat_fails: 3,
                fetch_limit: 64,
                cluster_token: Some("shared".into()),
                epoch: 2,
            },
            sink,
            0,
        );
        let (seq, _applied, failed, _perm) = follower.sync_once().await.unwrap();
        assert_eq!(seq, 0);
        assert!(!failed);
        leader_task.abort();
    }

    #[tokio::test]
    async fn bind_refuses_non_loopback_without_token() {
        // P1-6: 0.0.0.0 绑定 + 无 token → BindRequiresToken, 不 bind。
        let source = Arc::new(StubSource {
            entries: Mutex::new(vec![]),
        });
        let leader = Leader::new(source, 0).with_bind_addr("0.0.0.0");
        let err = leader.bind().await;
        assert!(err.is_err(), "non-loopback bind without token must refuse");
        match err.unwrap_err() {
            ClusterError::BindRequiresToken { addr } => {
                assert_eq!(addr, "0.0.0.0");
            }
            other => panic!("expected BindRequiresToken, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bind_refuses_non_loopback_even_with_allow_no_token() {
        // P1-6: allow_no_token 不豁免非 loopback (跨机必鉴权, 该 flag 仅单机测试)。
        let source = Arc::new(StubSource {
            entries: Mutex::new(vec![]),
        });
        let leader = Leader::new(source, 0)
            .with_bind_addr("10.0.0.5")
            .with_allow_no_token(true);
        let err = leader.bind().await;
        assert!(
            matches!(err, Err(ClusterError::BindRequiresToken { .. })),
            "allow_no_token must not bypass non-loopback gate, got {err:?}"
        );
    }

    #[tokio::test]
    async fn bind_allows_non_loopback_with_token() {
        // P1-6: 0.0.0.0 + 配 token → 放行 bind (实际绑 0 端口, OS 分配)。
        let source = Arc::new(StubSource {
            entries: Mutex::new(vec![]),
        });
        let leader = Leader::new(source, 0)
            .with_bind_addr("0.0.0.0")
            .with_token(Some("cluster-secret".into()));
        let res = leader.bind().await;
        assert!(res.is_ok(), "non-loopback + token should bind, got {res:?}");
    }

    #[tokio::test]
    async fn bind_allows_loopback_without_token() {
        // P1-6: loopback 127.0.0.1 + 无 token → 放行 (向后兼容, 单机 loopback 风险低)。
        let source = Arc::new(StubSource {
            entries: Mutex::new(vec![]),
        });
        let leader = Leader::new(source, 0); // 默认 bind_addr=127.0.0.1
        let res = leader.bind().await;
        assert!(
            res.is_ok(),
            "loopback bind without token should pass, got {res:?}"
        );
    }

    #[test]
    fn backoff_jitter_is_deterministic_and_bounded() {
        // §3.19: jitter 确定性 (无 rand), 落 [0, base/4], base<=1 → 0。
        assert_eq!(backoff_jitter(1, 5), 0, "base=1 no jitter");
        assert_eq!(backoff_jitter(0, 5), 0, "base=0 no jitter");
        // 同 seed 同 base → 同结果 (确定性)
        let a = backoff_jitter(60, 42);
        let b = backoff_jitter(60, 42);
        assert_eq!(a, b, "deterministic for same seed");
        // 界: base=60 → span=15 → jitter ∈ [0,15]
        assert!(a <= 15, "jitter within base/4 bound, got {a}");
        // 不同 seed → 可能不同 (散开, 避雷鸣群)。至少验证不恒等 (多个 seed 采样)。
        let seeds: Vec<i64> = (1..20).collect();
        let jitters: Vec<u64> = seeds.iter().map(|&s| backoff_jitter(60, s)).collect();
        assert!(
            jitters.iter().any(|&j| j != jitters[0]),
            "different seeds yield spread jitter"
        );
    }

    // 简单计数 sink, 记 tombstone/put/vector
    use fm_core::MemoryItem;
    struct CountingSink {
        tomb: Mutex<Vec<String>>,
    }
    impl CountingSink {
        fn new() -> Self {
            Self {
                tomb: Mutex::new(Vec::new()),
            }
        }
        fn tombstones(&self) -> Vec<String> {
            self.tomb.lock().unwrap().clone()
        }
    }
    #[async_trait::async_trait]
    impl ReplaySink for CountingSink {
        async fn embed(&self, _c: &str) -> ClusterResult<Vec<f32>> {
            Ok(vec![0.1; 2])
        }
        async fn put_item(&self, _i: &MemoryItem) -> ClusterResult<()> {
            Ok(())
        }
        async fn insert_vector(&self, _id: u64, _v: &[f32]) -> ClusterResult<()> {
            Ok(())
        }
        async fn tombstone(&self, id: &str) -> ClusterResult<()> {
            self.tomb.lock().unwrap().push(id.to_string());
            Ok(())
        }
    }
}
