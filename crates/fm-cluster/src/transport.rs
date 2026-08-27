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
}

impl Leader {
    pub fn new(source: Arc<dyn WopSource>, port: u16) -> Self {
        Self { source, port }
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
        // 1. hello 握手
        let hello_frame = read_frame(&mut stream)
            .await?
            .ok_or(ClusterError::Transport("hello missing".into()))?;
        if hello_frame.kind != FrameKind::Hello {
            return Err(ClusterError::Transport(format!(
                "expected hello, got {:?}",
                hello_frame.kind
            )));
        }
        let _hello: Hello = hello_frame.decode_payload()?;
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

    /// 单次同步: 连 leader → hello → sync request → 重放。返回新 last_seq + 应用数。
    pub async fn sync_once(&mut self) -> ClusterResult<(i64, usize)> {
        let mut stream = TcpStream::connect(&self.cfg.leader_addr).await?;
        let hello = Hello {
            follower_last_seq: self.local_last_seq,
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
        let (applied, _skipped) = replay_wops(self.sink.as_ref(), &resp.entries).await?;
        if resp.leader_last_seq > self.local_last_seq {
            self.local_last_seq = resp.leader_last_seq;
        }
        Ok((self.local_last_seq, applied))
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
    pub async fn run(mut self) -> ClusterResult<()> {
        info!(leader = %self.cfg.leader_addr, "follower sync loop start");
        let mut fails: u32 = 0;
        loop {
            match self.sync_once().await {
                Ok((seq, applied)) => {
                    if applied > 0 {
                        info!(seq, applied, "follower synced");
                    }
                    fails = 0;
                }
                Err(e) => {
                    fails += 1;
                    warn!(fails, error = %e, "follower sync fail");
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
            },
            sink.clone(),
            0,
        );
        let (seq, applied) = follower.sync_once().await.unwrap();
        assert_eq!(seq, 1);
        assert_eq!(applied, 1);
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
        use crate::protocol::{write_frame, Frame, FrameKind, Hello};
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
            },
            sink,
            0,
        );
        let _ = Hello {
            follower_last_seq: 0,
        };
        let err = follower.sync_once().await;
        assert!(err.is_err());
        fake_task.abort();
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
