//! fm-cluster: M6 集群同步。leader-follower wop_log replay。PRD §16。
//!
//! 角色 standalone(默认)/leader/follower，经 FUSION_MEMORY_ROLE 配置。
//! 写单点 leader，读本地，最终一致。内网 TCP 11436 (FUSION_MEMORY_SYNC_PORT)。
//! follower 连 FUSION_MEMORY_LEADER=host:port，拉 wop 增量本地重放。
//! 手动 failover (fm-cli cluster promote)，自动选举延后。

pub mod config;
pub mod error;
pub mod protocol;
pub mod replay;
pub mod role;
pub mod transport;

pub use config::{ClusterConfig, SyncConfig};
pub use error::{ClusterError, ClusterResult};
pub use protocol::{Frame, FrameKind, Hello, SyncRequest, SyncResponse};
pub use replay::{replay_wops, ReplaySink, WopSource};
pub use role::{detect_role, detect_role_with_home, write_role_file, NodeRole};
pub use transport::{Follower, Leader};
