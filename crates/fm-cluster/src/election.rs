//! v1.0.0 B-2: 自动 failover 选举。leader-lease + term + quorum 投票。PRD §16 商用阻断修复。
//!
//! 替代手动 `fm cluster promote`: leader 宕机 → follower 自动竞选新 leader。
//! 无 openraft 依赖 (openraft 仅 alpha, 商用风险), 自包含精简实现 (Rule 2):
//!
//! 算法 (Raft 本质, 非全量 Raft):
//! - **租约**: leader 周期发心跳 (复用 transport Ping), follower 记 last_heartbeat。
//!   租约期 = heartbeat_secs × heartbeat_fails (复用 SyncConfig)。
//! - **竞选触发**: follower 检测 now - last_heartbeat > lease → 转 candidate → 发起投票。
//! - **投票** (新增 FrameKind::VoteRequest/VoteResponse): candidate 向所有已知节点请求投票,
//!   携 (term, candidate_id, last_log_seq)。节点授权条件:
//!     1. candidate.term ≥ own.term
//!     2. candidate.last_log_seq ≥ own.last_log_seq (日志足够新, 复用 wop_log)
//!     3. 本 term 未投过票 (或已投给该 candidate)
//!
//!   quorum = 节点多数。
//! - **胜出 → promote**: candidate 拿 quorum → epoch++ (write_epoch_file, 复用 §1.8 fencing),
//!   转 leader (write_role_file + 起 Leader::serve)。
//!   旧 leader 复活后 epoch 较低 → follower StaleLeader 拒同步 (防脑裂双写)。
//! - **优先级**: 节点列表下标小者优先 (确定性, 避随机, 同 term 平票有定论)。
//!
//! 成员: 静态, env FUSION_MEMORY_CLUSTER_NODES=host:port,host:port,... (全节点), 自身下标
//! FUSION_MEMORY_CLUSTER_NODE_ID (0-based)。未配 → 无选举 (单机/手动模式兼容)。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::error::{ClusterError, ClusterResult};
use crate::protocol::{read_frame, write_frame, Frame, FrameKind};

/// v1.0.0 B-2: 本地日志 seq 提供者 (投票判"日志足够新"用)。MemoryEngine 经适配实现。
/// 解耦: election 不直接依赖 WopSource/ReplaySink (leader/follower 各持一个), 单一方法够。
#[async_trait]
pub trait LogSeqProvider: Send + Sync {
    async fn last_log_seq(&self) -> ClusterResult<i64>;
}

/// v1.0.0 B-2: 选举配置。静态成员 + 租约期 + quorum。
#[derive(Debug, Clone)]
pub struct ElectionConfig {
    /// 全节点地址列表 (FUSION_MEMORY_CLUSTER_NODES 解析)。索引即优先级 (小者高)。
    pub nodes: Vec<String>,
    /// 自身节点下标 (FUSION_MEMORY_CLUSTER_NODE_ID)。范围检查在 from_env。
    pub self_id: usize,
    /// 心跳间隔 (秒)。复用 SyncConfig::heartbeat_secs。
    pub heartbeat_secs: u64,
    /// 心跳失败阈值 (连续 N 失败 = leader down)。复用 SyncConfig::heartbeat_fails。
    pub heartbeat_fails: u32,
    /// 集群共享 token (鉴权, 复用 ClusterConfig::cluster_token)。竞选投票也校验。
    pub cluster_token: Option<String>,
}

impl ElectionConfig {
    /// quorum = floor(nodes/2) + 1。
    pub fn quorum(&self) -> usize {
        self.nodes.len() / 2 + 1
    }

    /// 租约期 = heartbeat_secs × heartbeat_fails (leader 超此无心跳 = down)。
    pub fn lease(&self) -> Duration {
        Duration::from_secs(self.heartbeat_secs * self.heartbeat_fails as u64)
    }
}

/// v1.0.0 B-2: env 常量。
pub const ENV_CLUSTER_NODES: &str = "FUSION_MEMORY_CLUSTER_NODES";
pub const ENV_CLUSTER_NODE_ID: &str = "FUSION_MEMORY_CLUSTER_NODE_ID";

impl ElectionConfig {
    /// 从 env 解析。FUSION_MEMORY_CLUSTER_NODES=host:port,host:port,...; 自身下标
    /// FUSION_MEMORY_CLUSTER_NODE_ID。未配 NODES → None (单机/手动模式, 不启用选举)。
    /// self_id 越界 → Err (配置错, 启动即败露非运行时静默, Rule 12)。
    /// heartbeat/token 复用 SyncConfig 默认 (5s/3) + ClusterConfig token, 也可独立 env。
    pub fn from_env() -> ClusterResult<Option<Self>> {
        let nodes_str = match std::env::var(ENV_CLUSTER_NODES) {
            Ok(s) if !s.trim().is_empty() => s,
            _ => return Ok(None),
        };
        let nodes: Vec<String> = nodes_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if nodes.len() < 2 {
            warn!(
                count = nodes.len(),
                "cluster nodes < 2, election disabled (need quorum)"
            );
            return Ok(None);
        }
        let self_id: usize = std::env::var(ENV_CLUSTER_NODE_ID)
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                ClusterError::Transport(format!(
                    "{ENV_CLUSTER_NODE_ID} missing/invalid (need 0-based index into {ENV_CLUSTER_NODES})"
                ))
            })?;
        if self_id >= nodes.len() {
            return Err(ClusterError::Transport(format!(
                "{ENV_CLUSTER_NODE_ID}={self_id} out of range (nodes={})",
                nodes.len()
            )));
        }
        let heartbeat_secs = std::env::var("FUSION_MEMORY_HEARTBEAT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(crate::config::DEFAULT_HEARTBEAT_SECS);
        let heartbeat_fails = std::env::var("FUSION_MEMORY_HEARTBEAT_FAILS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(crate::config::DEFAULT_HEARTBEAT_FAILS);
        let cluster_token = std::env::var(crate::config::ENV_CLUSTER_TOKEN)
            .ok()
            .filter(|t| !t.is_empty());
        Ok(Some(Self {
            nodes,
            self_id,
            heartbeat_secs,
            heartbeat_fails,
            cluster_token,
        }))
    }
}

/// v1.0.0 B-2: 投票请求 (新增帧 payload)。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VoteRequest {
    pub term: u64,
    pub candidate_id: usize,
    pub candidate_last_seq: i64,
    #[serde(default)]
    pub token: String,
}

/// v1.0.0 B-2: 投票响应。granted=true = 授权。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VoteResponse {
    pub term: u64,
    pub granted: bool,
    /// 授权方当前 last_seq (供 candidate 自检日志是否真领先)。
    #[serde(default)]
    pub voter_last_seq: i64,
}

/// v1.0.0 B-2: 节点选举状态机。单节点持有, leader/follower 共用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElectionState {
    Follower,
    Candidate,
    Leader,
}

/// v1.0.0 B-2: 选举运行时状态。投票授权判据依赖此 (term + voted_for + last_seq)。
#[derive(Debug, Clone)]
pub struct ElectionRuntime {
    pub state: ElectionState,
    pub current_term: u64,
    pub voted_for: Option<usize>,
    pub last_heartbeat: tokio::time::Instant,
    pub last_seq: i64,
}

impl Default for ElectionRuntime {
    fn default() -> Self {
        Self {
            state: ElectionState::Follower,
            current_term: 0,
            voted_for: None,
            last_heartbeat: tokio::time::Instant::now(),
            last_seq: 0,
        }
    }
}

/// v1.0.0 B-2: 选举器。持有配置 + 共享状态 (Arc<Mutex>)。投票 handler 与竞选循环共用。
pub struct Election {
    cfg: ElectionConfig,
    state: Arc<Mutex<ElectionRuntime>>,
}

impl Election {
    pub fn new(cfg: ElectionConfig) -> Self {
        Self {
            cfg,
            state: Arc::new(Mutex::new(ElectionRuntime::default())),
        }
    }

    pub fn cfg(&self) -> &ElectionConfig {
        &self.cfg
    }

    pub fn state_handle(&self) -> Arc<Mutex<ElectionRuntime>> {
        Arc::clone(&self.state)
    }

    /// v1.0.0 B-2: 处理收到的投票请求。授权判据 (Raft):
    /// 1. req.term < own.term → 拒 (旧 term)
    /// 2. req.term > own.term → 更新 own.term, 重置 voted_for (新 term 可投票)
    /// 3. own.voted_for 已投别人 (本 term) → 拒
    /// 4. req.candidate_last_seq < own.last_seq → 拒 (候选日志落后, 不够新)
    ///
    /// 通过 → 记 voted_for=candidate, granted=true。
    /// 返回 VoteResponse (own.term + granted + own.last_seq)。
    pub async fn handle_vote(
        &self,
        req: VoteRequest,
        local_seq: i64,
    ) -> ClusterResult<VoteResponse> {
        // token 校验 (复用 H3 鉴权): 不一致拒 (防未授权节点搅选举)。
        if let Some(expected) = &self.cfg.cluster_token {
            if !constant_time_eq(expected, &req.token) {
                warn!(
                    candidate = req.candidate_id,
                    "vote rejected: token mismatch"
                );
                let st = self.state.lock().await;
                return Ok(VoteResponse {
                    term: st.current_term,
                    granted: false,
                    voter_last_seq: st.last_seq,
                });
            }
        }
        let mut st = self.state.lock().await;
        // 判据 1/2: term 比较。
        if req.term > st.current_term {
            info!(
                old_term = st.current_term,
                new_term = req.term,
                "higher term seen, updating (new election)"
            );
            st.current_term = req.term;
            st.voted_for = None;
            st.state = ElectionState::Follower;
        }
        if req.term < st.current_term {
            return Ok(VoteResponse {
                term: st.current_term,
                granted: false,
                voter_last_seq: st.last_seq,
            });
        }
        // 判据 3: 本 term 已投别人。
        if let Some(voted) = st.voted_for {
            if voted != req.candidate_id {
                return Ok(VoteResponse {
                    term: st.current_term,
                    granted: false,
                    voter_last_seq: st.last_seq,
                });
            }
        }
        // 判据 4: 候选日志不够新。
        if req.candidate_last_seq < st.last_seq {
            warn!(
                candidate = req.candidate_id,
                candidate_last_seq = req.candidate_last_seq,
                own_last_seq = st.last_seq,
                "vote rejected: candidate log not up-to-date"
            );
            return Ok(VoteResponse {
                term: st.current_term,
                granted: false,
                voter_last_seq: st.last_seq,
            });
        }
        // 授权。
        st.voted_for = Some(req.candidate_id);
        // 投票 = 认可候选领导 → 重置租约 (候选将成 leader, 给它租约期不发起新竞选)。
        st.last_heartbeat = tokio::time::Instant::now();
        info!(
            term = st.current_term,
            candidate = req.candidate_id,
            "vote granted"
        );
        // 记录 local_seq 供响应 (用传入 local_seq, 非锁内旧值)。
        st.last_seq = st.last_seq.max(local_seq);
        Ok(VoteResponse {
            term: st.current_term,
            granted: true,
            voter_last_seq: st.last_seq,
        })
    }

    /// v1.0.0 B-2: 竞选。自增 term, 投自己, 向所有 peer 请求投票, 收 quorum → 胜出。
    /// 未达 quorum → false (下轮租约到期再试)。胜出 → 转 Leader, 返 true (调方起 serve)。
    pub async fn campaign(&self, seq_provider: Arc<dyn LogSeqProvider>) -> ClusterResult<bool> {
        let my_last_seq = seq_provider.last_log_seq().await?;
        let new_term;
        let quorum = self.cfg.quorum();
        {
            let mut st = self.state.lock().await;
            st.state = ElectionState::Candidate;
            st.current_term += 1;
            new_term = st.current_term;
            st.voted_for = Some(self.cfg.self_id);
            st.last_seq = st.last_seq.max(my_last_seq);
            info!(
                term = new_term,
                self_id = self.cfg.self_id,
                quorum,
                "starting campaign"
            );
        }
        let req = VoteRequest {
            term: new_term,
            candidate_id: self.cfg.self_id,
            candidate_last_seq: my_last_seq,
            token: self.cfg.cluster_token.clone().unwrap_or_default(),
        };
        // 向所有 peer (非自身) 并发请求投票。
        let peers: Vec<String> = self
            .cfg
            .nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != self.cfg.self_id)
            .map(|(_, addr)| addr.clone())
            .collect();
        let mut granted = 1usize; // 自投一票。
        for addr in peers {
            match request_vote(&addr, &req).await {
                Ok(resp) => {
                    if resp.granted {
                        granted += 1;
                        info!(peer = %addr, term = resp.term, "vote received");
                    } else {
                        info!(peer = %addr, term = resp.term, "vote denied");
                    }
                }
                Err(e) => {
                    warn!(peer = %addr, error = %e, "vote request failed (peer down?)");
                }
            }
        }
        if granted >= quorum {
            let mut st = self.state.lock().await;
            st.state = ElectionState::Leader;
            st.last_heartbeat = tokio::time::Instant::now();
            info!(
                term = new_term,
                granted, quorum, "WON election, becoming leader"
            );
            Ok(true)
        } else {
            info!(granted, quorum, "lost election, staying follower/candidate");
            let mut st = self.state.lock().await;
            st.state = ElectionState::Follower;
            Ok(false)
        }
    }

    /// v1.0.0 B-2: 租约到期检测。now - last_heartbeat > lease → true (leader down, 该竞选)。
    pub async fn lease_expired(&self) -> bool {
        let st = self.state.lock().await;
        if st.state == ElectionState::Leader {
            return false; // 自身是 leader, 无需竞选。
        }
        st.last_heartbeat.elapsed() > self.cfg.lease()
    }

    /// v1.0.0 B-2: leader/peer 收到心跳, 更新租约 (防本节点误判 leader down 发起竞选)。
    pub async fn refresh_lease(&self, seq: i64) {
        let mut st = self.state.lock().await;
        st.last_heartbeat = tokio::time::Instant::now();
        st.last_seq = st.last_seq.max(seq);
    }
}

/// v1.0.0 B-2: 向单个 peer 发投票请求。hello(token) + VoteRequest → VoteResponse。
/// 复用线帧协议 (新增 VoteRequest/VoteResponse 帧类型)。
async fn request_vote(addr: &str, req: &VoteRequest) -> ClusterResult<VoteResponse> {
    let mut stream = TcpStream::connect(addr).await?;
    // hello 握手带 token (leader 端 vote handler 也校验 token, 复用 Hello)。
    let hello = crate::protocol::Hello {
        follower_last_seq: req.candidate_last_seq,
        token: req.token.clone(),
        epoch: 0,
    };
    write_frame(&mut stream, &Frame::new(FrameKind::Hello, hello)?).await?;
    write_frame(
        &mut stream,
        &Frame::new(FrameKind::VoteRequest, req.clone())?,
    )
    .await?;
    let resp_frame = read_frame(&mut stream)
        .await?
        .ok_or(ClusterError::Transport("vote response missing".into()))?;
    if resp_frame.kind != FrameKind::VoteResponse {
        return Err(ClusterError::Transport(format!(
            "expected vote response, got {:?}",
            resp_frame.kind
        )));
    }
    resp_frame.decode_payload()
}

/// 常时比较 token (复用 transport 同逻辑, 防 timing attack)。
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

/// v1.0.0 B-2: 投票监听器。每个集群节点 (leader + follower) 各起一个, 绑 nodes[self_id]。
/// 接收 Hello(token 握手) + VoteRequest → handle_vote → 回 VoteResponse。
/// follower 也需监听: 候选向所有 peer 请求投票, peer 须能收 (仅 leader 监听则 leader 宕时无人可投)。
/// cancel-safe: 每连接独立 task。调方 spawn 此 fn (阻塞 accept 循环)。
pub async fn serve_votes(
    listener: tokio::net::TcpListener,
    election: Arc<Election>,
    seq_provider: Arc<dyn LogSeqProvider>,
) -> ClusterResult<()> {
    let addr = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "?".into());
    info!(%addr, "vote listener bound");
    loop {
        match listener.accept().await {
            Ok((mut stream, peer)) => {
                let el = Arc::clone(&election);
                let sp = Arc::clone(&seq_provider);
                tokio::spawn(async move {
                    if let Err(e) = handle_vote_conn(&mut stream, el, sp).await {
                        warn!(%peer, error = %e, "vote conn ended");
                    }
                });
            }
            Err(e) => {
                error!(error = %e, "vote listener accept failed");
                return Err(e.into());
            }
        }
    }
}

/// 单条投票连接处理: 读 Hello (握手, 复用 token 鉴权) → 读 VoteRequest → handle_vote → 回 VoteResponse。
async fn handle_vote_conn(
    stream: &mut TcpStream,
    election: Arc<Election>,
    seq_provider: Arc<dyn LogSeqProvider>,
) -> ClusterResult<()> {
    // Hello 握手 (复用线帧, candidate request_vote 先发 Hello 再发 VoteRequest)。
    let hello_frame = read_frame(stream)
        .await?
        .ok_or(ClusterError::Transport("vote hello missing".into()))?;
    if hello_frame.kind != FrameKind::Hello {
        return Err(ClusterError::Transport(format!(
            "expected hello, got {:?}",
            hello_frame.kind
        )));
    }
    let _hello: crate::protocol::Hello = hello_frame.decode_payload()?;
    // VoteRequest (handle_vote 内部校验 token, 此处不重复)。
    let req_frame = read_frame(stream)
        .await?
        .ok_or(ClusterError::Transport("vote request missing".into()))?;
    if req_frame.kind != FrameKind::VoteRequest {
        return Err(ClusterError::Transport(format!(
            "expected vote request, got {:?}",
            req_frame.kind
        )));
    }
    let req: VoteRequest = req_frame.decode_payload()?;
    let local_seq = seq_provider.last_log_seq().await?;
    let resp = election.handle_vote(req, local_seq).await?;
    write_frame(stream, &Frame::new(FrameKind::VoteResponse, resp)?).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    // env 全局, 并行测试串扰 → 互斥锁串行化 (同 config.rs/role.rs 模式)。
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    #[test]
    fn quorum_majority() {
        let cfg = ElectionConfig {
            nodes: vec!["a".into(), "b".into(), "c".into()],
            self_id: 0,
            heartbeat_secs: 5,
            heartbeat_fails: 3,
            cluster_token: None,
        };
        assert_eq!(cfg.quorum(), 2, "3 nodes → quorum 2");
        let cfg5 = ElectionConfig {
            nodes: vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()],
            self_id: 0,
            heartbeat_secs: 5,
            heartbeat_fails: 3,
            cluster_token: None,
        };
        assert_eq!(cfg5.quorum(), 3, "5 nodes → quorum 3");
    }

    #[test]
    fn lease_is_heartbeat_times_fails() {
        let cfg = ElectionConfig {
            nodes: vec!["a".into(), "b".into()],
            self_id: 0,
            heartbeat_secs: 5,
            heartbeat_fails: 3,
            cluster_token: None,
        };
        assert_eq!(cfg.lease(), Duration::from_secs(15));
    }

    #[test]
    fn election_state_default_follower() {
        let s = ElectionRuntime::default();
        assert_eq!(s.state, ElectionState::Follower);
        assert_eq!(s.current_term, 0);
        assert!(s.voted_for.is_none());
    }

    // v1.0.0 B-2: handle_vote 授权/拒绝判定。
    #[tokio::test]
    async fn handle_vote_grants_higher_term_uptodate_log() {
        // term=1 > own=0, candidate_seq=5 ≥ own=0, 未投 → 授权。
        let el = Election::new(ElectionConfig {
            nodes: vec!["a".into(), "b".into()],
            self_id: 0,
            heartbeat_secs: 5,
            heartbeat_fails: 3,
            cluster_token: None,
        });
        let req = VoteRequest {
            term: 1,
            candidate_id: 1,
            candidate_last_seq: 5,
            token: String::new(),
        };
        let resp = el.handle_vote(req, 0).await.unwrap();
        assert!(resp.granted, "higher term + uptodate log → grant");
        assert_eq!(resp.term, 1, "own term updated to candidate term");
    }

    #[tokio::test]
    async fn handle_vote_denies_lower_term() {
        // own term 已被推到 2 (前一轮授权), 候选 term=1 < 2 → 拒。
        let el = Election::new(ElectionConfig {
            nodes: vec!["a".into(), "b".into()],
            self_id: 0,
            heartbeat_secs: 5,
            heartbeat_fails: 3,
            cluster_token: None,
        });
        // 先推 own term 到 2 (模拟见过 term 2 选举)
        el.handle_vote(
            VoteRequest {
                term: 2,
                candidate_id: 1,
                candidate_last_seq: 0,
                token: String::new(),
            },
            0,
        )
        .await
        .unwrap();
        let resp = el
            .handle_vote(
                VoteRequest {
                    term: 1,
                    candidate_id: 1,
                    candidate_last_seq: 0,
                    token: String::new(),
                },
                0,
            )
            .await
            .unwrap();
        assert!(!resp.granted, "lower term → deny");
        assert_eq!(resp.term, 2, "own term stays 2");
    }

    #[tokio::test]
    async fn handle_vote_denies_already_voted_other() {
        // 本 term 已投 candidate 1, candidate 2 同 term 来 → 拒 (一 term 一投)。
        let el = Election::new(ElectionConfig {
            nodes: vec!["a".into(), "b".into(), "c".into()],
            self_id: 0,
            heartbeat_secs: 5,
            heartbeat_fails: 3,
            cluster_token: None,
        });
        el.handle_vote(
            VoteRequest {
                term: 1,
                candidate_id: 1,
                candidate_last_seq: 0,
                token: String::new(),
            },
            0,
        )
        .await
        .unwrap();
        let resp = el
            .handle_vote(
                VoteRequest {
                    term: 1,
                    candidate_id: 2,
                    candidate_last_seq: 0,
                    token: String::new(),
                },
                0,
            )
            .await
            .unwrap();
        assert!(!resp.granted, "already voted other this term → deny");
    }

    #[tokio::test]
    async fn handle_vote_denies_stale_log() {
        // own last_seq=10, candidate_seq=3 < 10 → 拒 (候选日志落后)。
        let el = Election::new(ElectionConfig {
            nodes: vec!["a".into(), "b".into()],
            self_id: 0,
            heartbeat_secs: 5,
            heartbeat_fails: 3,
            cluster_token: None,
        });
        // 注入 own last_seq=10 (经 refresh_lease 或 handle_vote 的 local_seq 参数)
        el.refresh_lease(10).await;
        let resp = el
            .handle_vote(
                VoteRequest {
                    term: 5,
                    candidate_id: 1,
                    candidate_last_seq: 3,
                    token: String::new(),
                },
                10,
            )
            .await
            .unwrap();
        assert!(!resp.granted, "candidate log stale → deny");
    }

    #[tokio::test]
    async fn handle_vote_denies_token_mismatch() {
        // 配 token, 候选带错 token → 拒 (防未授权节点搅选举)。
        let el = Election::new(ElectionConfig {
            nodes: vec!["a".into(), "b".into()],
            self_id: 0,
            heartbeat_secs: 5,
            heartbeat_fails: 3,
            cluster_token: Some("shared-secret".into()),
        });
        let resp = el
            .handle_vote(
                VoteRequest {
                    term: 1,
                    candidate_id: 1,
                    candidate_last_seq: 0,
                    token: "wrong-token".into(),
                },
                0,
            )
            .await
            .unwrap();
        assert!(!resp.granted, "token mismatch → deny");
    }

    #[tokio::test]
    async fn lease_expired_after_lease_duration() {
        // lease = 5×3 = 15s。模拟 last_heartbeat 久远 → expired。
        let el = Election::new(ElectionConfig {
            nodes: vec!["a".into(), "b".into()],
            self_id: 0,
            heartbeat_secs: 1,
            heartbeat_fails: 1,
            cluster_token: None,
        });
        // lease = 1s。sleep 1.1s 后应 expired。
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(el.lease_expired().await, "lease elapsed → expired");
        // refresh 后不再 expired。
        el.refresh_lease(0).await;
        assert!(!el.lease_expired().await, "just refreshed → not expired");
    }

    #[tokio::test]
    async fn lease_not_expired_for_leader() {
        // 自身是 leader → 永不 expired (无需竞选)。
        let el = Election::new(ElectionConfig {
            nodes: vec!["a".into(), "b".into()],
            self_id: 0,
            heartbeat_secs: 1,
            heartbeat_fails: 1,
            cluster_token: None,
        });
        // 手动设 leader 状态
        {
            let h = el.state_handle();
            let mut st = h.lock().await;
            st.state = ElectionState::Leader;
        }
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!el.lease_expired().await, "leader never expires");
    }

    // v1.0.0 B-2: serve_votes + request_vote 端到端 (真实 TCP vote listener)。
    // 起 2 节点 vote listener, candidate 向 peer 请求投票, peer 授权 (term 高 + log 新)。
    struct StaticSeq {
        seq: i64,
    }
    #[async_trait]
    impl LogSeqProvider for StaticSeq {
        async fn last_log_seq(&self) -> ClusterResult<i64> {
            Ok(self.seq)
        }
    }

    #[tokio::test]
    async fn campaign_wins_quorum_with_live_vote_listeners() {
        // 3 节点: candidate=self_id=0, peers=1,2 各起 vote listener (返授权)。
        // candidate 竞选 → 自投 1 + 2 peer = 3 ≥ quorum 2 → 胜出。
        let listener1 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr1 = listener1.local_addr().unwrap().to_string();
        let addr2 = listener2.local_addr().unwrap().to_string();

        // peer 1 vote listener (空日志, term 0 → 见 candidate term 1, 授权)
        let el1 = Arc::new(Election::new(ElectionConfig {
            nodes: vec![addr1.clone(), addr2.clone()], // 占位, 实际 peer 用各自 cfg
            self_id: 0,
            heartbeat_secs: 5,
            heartbeat_fails: 3,
            cluster_token: None,
        }));
        let sp1: Arc<dyn LogSeqProvider> = Arc::new(StaticSeq { seq: 0 });
        let t1 = tokio::spawn(serve_votes(listener1, el1, sp1));
        let el2 = Arc::new(Election::new(ElectionConfig {
            nodes: vec![addr1.clone(), addr2.clone()],
            self_id: 0,
            heartbeat_secs: 5,
            heartbeat_fails: 3,
            cluster_token: None,
        }));
        let sp2: Arc<dyn LogSeqProvider> = Arc::new(StaticSeq { seq: 0 });
        let t2 = tokio::spawn(serve_votes(listener2, el2, sp2));

        // candidate: self_id=0, nodes[0]=占位 (自身, 不连), nodes[1]=addr1, nodes[2]=addr2。
        // 但 request_vote 连的是 nodes 里非自身下标 → 用 addr1/addr2 当 peer。
        // candidate 自己的监听地址用占位 (不 bind, 不连自身)。
        let cand_cfg = ElectionConfig {
            nodes: vec!["127.0.0.1:1".into(), addr1, addr2],
            self_id: 0,
            heartbeat_secs: 5,
            heartbeat_fails: 3,
            cluster_token: None,
        };
        let candidate = Election::new(cand_cfg);
        let cand_seq: Arc<dyn LogSeqProvider> = Arc::new(StaticSeq { seq: 0 });
        let won = candidate.campaign(cand_seq).await.unwrap();
        assert!(won, "3 nodes, 2 peers grant → quorum 2 → win");
        let h = candidate.state_handle();
        let st = h.lock().await;
        assert_eq!(st.state, ElectionState::Leader, "won → became leader");
        assert_eq!(st.current_term, 1, "first campaign → term 1");
        t1.abort();
        t2.abort();
    }

    #[tokio::test]
    async fn campaign_loses_when_quorum_unreachable() {
        // 3 节点: candidate=self_id=0, peers=1,2 都不在线 (vote listener 未起)。
        // 自投 1, peer 连接失败 → granted=1 < quorum 2 → 败。
        let cand_cfg = ElectionConfig {
            nodes: vec![
                "127.0.0.1:1".into(),
                "127.0.0.1:9".into(), // 未监听 → 连接失败
                "127.0.0.1:8".into(), // 未监听 → 连接失败
            ],
            self_id: 0,
            heartbeat_secs: 5,
            heartbeat_fails: 3,
            cluster_token: None,
        };
        let candidate = Election::new(cand_cfg);
        let cand_seq: Arc<dyn LogSeqProvider> = Arc::new(StaticSeq { seq: 0 });
        let won = candidate.campaign(cand_seq).await.unwrap();
        assert!(!won, "peers unreachable → no quorum → lose");
        let h = candidate.state_handle();
        let st = h.lock().await;
        assert_eq!(st.state, ElectionState::Follower, "lost → back to follower");
    }

    #[tokio::test]
    async fn from_env_no_nodes_returns_none() {
        // 未配 FUSION_MEMORY_CLUSTER_NODES → None (单机模式, 不启用选举)。
        let _g = lock();
        std::env::remove_var(ENV_CLUSTER_NODES);
        assert!(ElectionConfig::from_env().unwrap().is_none());
    }

    #[tokio::test]
    async fn from_env_single_node_returns_none() {
        // 仅 1 节点 → 无 quorum 意义 → None (不启用选举)。
        let _g = lock();
        std::env::set_var(ENV_CLUSTER_NODES, "127.0.0.1:11436");
        std::env::set_var(ENV_CLUSTER_NODE_ID, "0");
        assert!(
            ElectionConfig::from_env().unwrap().is_none(),
            "1 node → no election"
        );
        std::env::remove_var(ENV_CLUSTER_NODES);
        std::env::remove_var(ENV_CLUSTER_NODE_ID);
    }

    #[tokio::test]
    async fn from_env_self_id_out_of_range_errors() {
        // self_id 越界 → Err (配置错, 启动即败露, Rule 12)。
        let _g = lock();
        std::env::set_var(ENV_CLUSTER_NODES, "127.0.0.1:11436,127.0.0.1:11437");
        std::env::set_var(ENV_CLUSTER_NODE_ID, "5");
        let res = ElectionConfig::from_env();
        assert!(res.is_err(), "self_id out of range → error");
        std::env::remove_var(ENV_CLUSTER_NODES);
        std::env::remove_var(ENV_CLUSTER_NODE_ID);
    }

    #[tokio::test]
    async fn from_env_valid_two_nodes_parses() {
        let _g = lock();
        std::env::set_var(ENV_CLUSTER_NODES, "127.0.0.1:11436,127.0.0.1:11437");
        std::env::set_var(ENV_CLUSTER_NODE_ID, "0");
        let cfg = ElectionConfig::from_env().unwrap().unwrap();
        assert_eq!(cfg.nodes.len(), 2);
        assert_eq!(cfg.self_id, 0);
        assert_eq!(cfg.quorum(), 2);
        std::env::remove_var(ENV_CLUSTER_NODES);
        std::env::remove_var(ENV_CLUSTER_NODE_ID);
    }
}
