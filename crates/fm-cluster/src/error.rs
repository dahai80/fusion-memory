use thiserror::Error;

pub type ClusterResult<T> = Result<T, ClusterError>;

#[derive(Debug, Error)]
pub enum ClusterError {
    #[error("cluster io: {0}")]
    Io(#[from] std::io::Error),
    #[error("cluster serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("cluster persist: {0}")]
    Persist(#[from] fm_persist::PersistError),
    #[error("cluster memory: {0}")]
    Memory(#[from] fm_core::MemoryError),
    #[error("cluster replay: {0}")]
    Replay(String),
    #[error("cluster transport: {0}")]
    Transport(String),
    #[error("leader down: {0} consecutive heartbeat fails")]
    LeaderDown(u32),
    #[error("not a leader (role={0})")]
    NotLeader(String),
    #[error("not a follower (role={0})")]
    NotFollower(String),
    /// §1.8: leader epoch < follower 期望 → 陈旧 leader, 拒同步防脑裂双写。
    #[error("stale leader epoch: leader={leader_epoch} < expected={expected_epoch}")]
    StaleLeader {
        leader_epoch: u64,
        expected_epoch: u64,
    },
    /// §3.19: 永久重放失败 (payload 损坏/未知 op/sink 拒绝)。不应重试 — 与瞬时 (mlx 429/IO busy) 区分。
    #[error("permanent replay failure: {0}")]
    PermanentReplay(String),
    /// §2.2: 鉴权未配置 (无 token + 未显式放行) → fail-closed 拒连接。
    #[error(
        "cluster auth not configured: no token and FUSION_MEMORY_CLUSTER_ALLOW_NO_TOKEN unset"
    )]
    AuthNotConfigured,
    /// P1-6: 非 loopback 绑定 (0.0.0.0/内网 IP) 但未配 cluster_token → 拒启动。
    /// 跨机暴露必须鉴权, 防 PII 明文外泄 + 重放攻击面。
    #[error("cluster bind to non-loopback {addr} requires FUSION_MEMORY_CLUSTER_TOKEN")]
    BindRequiresToken { addr: String },
}

impl ClusterError {
    /// §3.19: 是否永久错误 (重试无意义)。瞬时错误 (IO/serde/transport busy/mlx 429) 返回 false,
    /// 永久错误 (payload 损坏/未知 op/陈旧 epoch/鉴权配置) 返回 true。
    /// follower run 据此决定: 永久 → 升级 warn + 不再退避重试该批 (游标卡住, 等运维介入),
    /// 瞬时 → 退避重试 (leader 可能恢复)。
    pub fn is_permanent(&self) -> bool {
        matches!(
            self,
            ClusterError::PermanentReplay(_)
                | ClusterError::StaleLeader { .. }
                | ClusterError::AuthNotConfigured
                | ClusterError::BindRequiresToken { .. }
                | ClusterError::NotLeader(_)
                | ClusterError::NotFollower(_)
        )
    }
}
