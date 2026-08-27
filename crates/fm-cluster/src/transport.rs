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
    /// H3 鉴权: 配置则校验 follower hello.token, 不一致拒连接。None → 不校验 (仅 loopback)。
    cluster_token: Option<String>,
}

impl Leader {
    pub fn new(source: Arc<dyn WopSource>, port: u16) -> Self {
        Self {
            source,
            port,
            cluster_token: None,
        }
    }

    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.cluster_token = token;
        self
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// 绑定监听端口, 返回 TcpListener + 实际端口 (0→OS 分配)。serve_listener 复用。
    pub async fn bind(&self) -> ClusterResult<(TcpListener, u16)> {
        let listener = TcpListener::bind(format!("127.0.0.1:{}", self.port)).await?;
        let port = listener.local_addr()?.port();
        info!(port, "cluster leader bound");
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
        if let Some(expected) = &self.cluster_token {
            if !constant_time_eq(expected, &hello.token) {
                warn!("leader: follower token mismatch, rejecting conn");
                return Err(ClusterError::Transport("auth: token mismatch".into()));
            }
        }
        // 2. 循环响应 SyncRequest / Ping
        while let Some(frame) = read_frame(&mut stream).await? {
            match frame.kind {
                FrameKind::SyncRequest => {
                    let req: SyncRequest = frame.decode_payload()?;
                    let entries = self.source.list_wop_since(req.since_seq, req.limit)?;
                    let leader_last_seq = self.source.last_wop_seq()?;
                    let resp = SyncResponse {
                        entries,
                        leader_last_seq,
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

    /// 单次同步: 连 leader → hello(token) → sync request → 重放。
    /// 返回 (新 last_seq, 应用数, 本批是否含落地失败条目)。
    /// H2 修正: local_last_seq 推进到本批已落地条目的最大 seq (last_applied_seq),
    /// 而非 leader_last_seq。失败条目下轮重拉, 已落地条目不重复重放 (stub 幂等)。
    /// 传输层失败 (connect/帧坏/decode) 仍返 Err → caller 判 leader 宕机倾向。
    /// 单条落地失败 → Ok(_, _, true), 不抛 Err → caller 不触发 failover (非 leader 宕机)。
    pub async fn sync_once(&mut self) -> ClusterResult<(i64, usize, bool)> {
        let mut stream = TcpStream::connect(&self.cfg.leader_addr).await?;
        let hello = Hello {
            follower_last_seq: self.local_last_seq,
            token: self.cfg.cluster_token.clone().unwrap_or_default(),
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
        let outcome = replay_wops(self.sink.as_ref(), &resp.entries).await;
        // H2: 推进到 last_applied_seq (已落地/跳过最大 seq), 非 leader_last_seq。
        // 失败条目不计入 last_applied_seq → 游标卡在失败条目前, 下轮重拉。
        if outcome.last_applied_seq > self.local_last_seq {
            self.local_last_seq = outcome.last_applied_seq;
        }
        Ok((self.local_last_seq, outcome.applied, outcome.failed))
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
    pub async fn run(mut self) -> ClusterResult<()> {
        info!(leader = %self.cfg.leader_addr, "follower sync loop start");
        let mut fails: u32 = 0;
        let mut replay_backoff: u64 = 1;
        loop {
            match self.sync_once().await {
                Ok((_seq, applied, replay_failed)) => {
                    if applied > 0 {
                        info!(applied, "follower synced");
                    }
                    fails = 0;
                    if replay_failed {
                        // 连上 leader 但本批有单条落地失败 → 非 leader 宕机, 退避重试, 不触发 failover。
                        warn!("follower replay fail (leader alive), backoff retry");
                        replay_backoff = (replay_backoff * 2).min(60);
                        sleep(Duration::from_secs(replay_backoff)).await;
                        continue;
                    }
                    replay_backoff = 1;
                }
                Err(e) => {
                    // transport/io/serde → 连不上或帧坏, 真宕机倾向 → 计 fails。
                    fails += 1;
                    warn!(fails, error = %e, "follower sync fail (transport)");
                    if fails >= self.cfg.heartbeat_fails {
                        return Err(ClusterError::LeaderDown(fails));
                    }
                }
            }
            sleep(Duration::from_secs(self.cfg.heartbeat_secs)).await;
        }
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
        let leader = Arc::new(Leader::new(source, 0));
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
            },
            sink.clone(),
            0,
        );
        let (seq, applied, failed) = follower.sync_once().await.unwrap();
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
        let leader = Arc::new(Leader::new(source, 0));
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
        let leader = Arc::new(Leader::new(source, port));
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
        let leader = Arc::new(Leader::new(source, 0));
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
            },
            sink,
            0,
        );
        let (seq, _applied, failed) = follower.sync_once().await.unwrap();
        assert_eq!(seq, 0, "empty source, no advance");
        assert!(!failed);
        leader_task.abort();
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
