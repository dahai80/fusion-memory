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
}
