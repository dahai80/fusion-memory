use std::env;

pub const DEFAULT_SYNC_PORT: u16 = 11436;
pub const DEFAULT_HEARTBEAT_SECS: u64 = 5;
pub const DEFAULT_HEARTBEAT_FAILS: u32 = 3;
pub const DEFAULT_FETCH_LIMIT: usize = 256;
pub const ENV_CLUSTER_TOKEN: &str = "FUSION_MEMORY_CLUSTER_TOKEN";
/// §1.8: leader epoch env。手动 failover (fm cluster promote) 递增此值, follower 拒 epoch < 期望的 leader。
pub const ENV_CLUSTER_EPOCH: &str = "FUSION_MEMORY_CLUSTER_EPOCH";
/// §1.8: leader 绑定地址 env (跨机部署)。默认 127.0.0.1 (loopback 兼容)。内网集群设 0.0.0.0 或本机内网 IP。
pub const ENV_CLUSTER_BIND_ADDR: &str = "FUSION_MEMORY_CLUSTER_BIND_ADDR";
/// §2.2: 显式放行无 token 部署 (仅单机测试)。生产必须配 token, 此 env 不设时 leader 无 token → 拒所有连接。
pub const ENV_CLUSTER_ALLOW_NO_TOKEN: &str = "FUSION_MEMORY_CLUSTER_ALLOW_NO_TOKEN";

/// 集群全局配置 (角色 + 端口 + 绑定地址 + 鉴权 token + epoch)。
#[derive(Debug, Clone)]
pub struct ClusterConfig {
    pub sync_port: u16,
    /// §1.8: leader 绑定地址。默认 127.0.0.1 (loopback)。内网跨机部署设 0.0.0.0 或内网 IP。
    pub bind_addr: String,
    /// H3 鉴权: leader/follower 共享 secret。未配置 → §2.2 fail-closed: leader 拒所有连接
    /// (除非显式 FUSION_MEMORY_CLUSTER_ALLOW_NO_TOKEN=1, 仅单机测试可接受)。
    /// 生产部署必须配 FUSION_MEMORY_CLUSTER_TOKEN, leader/follower 一致, 否则 leader 拒非授权连接。
    pub cluster_token: Option<String>,
    /// §2.2: 无 token 显式放行。仅 ENV_CLUSTER_ALLOW_NO_TOKEN=1 时 true。生产禁用。
    pub allow_no_token: bool,
    /// §1.8: leader epoch。手动 failover 递增。follower hello.epoch 携带期望值, leader 自报,
    /// follower 拒 leader_epoch < 期望 (陈旧 leader fencing, 防分区双写脑裂)。
    pub epoch: u64,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            sync_port: env::var("FUSION_MEMORY_SYNC_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_SYNC_PORT),
            bind_addr: env::var(ENV_CLUSTER_BIND_ADDR)
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "127.0.0.1".to_string()),
            cluster_token: env::var(ENV_CLUSTER_TOKEN).ok().filter(|t| !t.is_empty()),
            allow_no_token: env::var(ENV_CLUSTER_ALLOW_NO_TOKEN)
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            epoch: env::var(ENV_CLUSTER_EPOCH)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
        }
    }
}

impl ClusterConfig {
    /// §1.8: env 优先, 次读 home/epoch 文件 (fm cluster promote 递增落地), 末 0 (不 fencing)。
    /// bind_addr/allow_no_token/token 仅 env (部署配置, 非 failover 状态), epoch 合并 home 文件。
    pub fn with_home(home: &std::path::Path) -> Self {
        let mut cfg = Self::default();
        if cfg.epoch == 0 {
            cfg.epoch = crate::role::read_epoch_file(home);
        }
        cfg
    }
}

/// follower 拉取/心跳配置。PRD §16: 心跳 5s, 3 失败 = leader down。
#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub leader_addr: String,
    pub heartbeat_secs: u64,
    pub heartbeat_fails: u32,
    pub fetch_limit: usize,
    /// H3 鉴权 token, follower 握手带上, leader 校验。
    pub cluster_token: Option<String>,
    /// §1.8: follower 期望的 leader epoch (来自 ENV_CLUSTER_EPOCH)。leader 自报 epoch,
    /// follower 拒 leader_epoch < 此值 (陈旧 leader fencing, 防分区双写脑裂)。0 = 不 fencing。
    pub epoch: u64,
}

impl SyncConfig {
    pub fn from_env() -> Option<Self> {
        let leader_addr = env::var("FUSION_MEMORY_LEADER").ok()?;
        Some(Self {
            leader_addr,
            heartbeat_secs: env::var("FUSION_MEMORY_HEARTBEAT_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_HEARTBEAT_SECS),
            heartbeat_fails: env::var("FUSION_MEMORY_HEARTBEAT_FAILS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_HEARTBEAT_FAILS),
            fetch_limit: env::var("FUSION_MEMORY_FETCH_LIMIT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_FETCH_LIMIT),
            cluster_token: env::var(ENV_CLUSTER_TOKEN).ok().filter(|t| !t.is_empty()),
            epoch: env::var(ENV_CLUSTER_EPOCH)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    // env 全局, 并行测试串扰 → 互斥锁串行化。
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    #[test]
    fn cluster_config_default_port() {
        let _g = lock();
        env::remove_var("FUSION_MEMORY_SYNC_PORT");
        env::remove_var(ENV_CLUSTER_BIND_ADDR);
        env::remove_var(ENV_CLUSTER_ALLOW_NO_TOKEN);
        env::remove_var(ENV_CLUSTER_EPOCH);
        let c = ClusterConfig::default();
        assert_eq!(c.sync_port, DEFAULT_SYNC_PORT);
        assert_eq!(c.bind_addr, "127.0.0.1", "default bind loopback");
        assert!(!c.allow_no_token, "default fail-closed no-token");
        assert_eq!(c.epoch, 0, "default epoch 0 (no fencing)");
    }

    #[test]
    fn cluster_config_env_override() {
        let _g = lock();
        env::set_var("FUSION_MEMORY_SYNC_PORT", "12000");
        assert_eq!(ClusterConfig::default().sync_port, 12000);
        env::remove_var("FUSION_MEMORY_SYNC_PORT");
    }

    #[test]
    fn cluster_config_bind_addr_override() {
        // §1.8: 内网跨机部署设 0.0.0.0 或内网 IP。
        let _g = lock();
        env::set_var(ENV_CLUSTER_BIND_ADDR, "0.0.0.0");
        assert_eq!(ClusterConfig::default().bind_addr, "0.0.0.0");
        env::remove_var(ENV_CLUSTER_BIND_ADDR);
    }

    #[test]
    fn cluster_config_epoch_override() {
        // §1.8: failover 递增 epoch。
        let _g = lock();
        env::set_var(ENV_CLUSTER_EPOCH, "7");
        assert_eq!(ClusterConfig::default().epoch, 7);
        env::remove_var(ENV_CLUSTER_EPOCH);
    }

    #[test]
    fn cluster_config_allow_no_token_opt_in() {
        // §2.2: 仅显式 ENV=1 放行无 token, 默认 fail-closed。
        let _g = lock();
        env::set_var(ENV_CLUSTER_ALLOW_NO_TOKEN, "1");
        assert!(ClusterConfig::default().allow_no_token);
        env::set_var(ENV_CLUSTER_ALLOW_NO_TOKEN, "false");
        assert!(!ClusterConfig::default().allow_no_token);
        env::remove_var(ENV_CLUSTER_ALLOW_NO_TOKEN);
    }

    #[test]
    fn sync_config_missing_leader_is_none() {
        let _g = lock();
        env::remove_var("FUSION_MEMORY_LEADER");
        assert!(SyncConfig::from_env().is_none());
    }

    #[test]
    fn sync_config_with_leader() {
        let _g = lock();
        env::set_var("FUSION_MEMORY_LEADER", "127.0.0.1:11436");
        let c = SyncConfig::from_env().unwrap();
        assert_eq!(c.leader_addr, "127.0.0.1:11436");
        assert_eq!(c.heartbeat_secs, DEFAULT_HEARTBEAT_SECS);
        assert_eq!(c.heartbeat_fails, DEFAULT_HEARTBEAT_FAILS);
        env::remove_var("FUSION_MEMORY_LEADER");
    }

    #[test]
    fn sync_config_env_override_all_fields() {
        // 覆盖 heartbeat_secs/heartbeat_fails/fetch_limit 环境变量覆盖分支。
        let _g = lock();
        env::set_var("FUSION_MEMORY_LEADER", "127.0.0.1:9");
        env::set_var("FUSION_MEMORY_HEARTBEAT_SECS", "2");
        env::set_var("FUSION_MEMORY_HEARTBEAT_FAILS", "5");
        env::set_var("FUSION_MEMORY_FETCH_LIMIT", "128");
        let c = SyncConfig::from_env().unwrap();
        assert_eq!(c.heartbeat_secs, 2);
        assert_eq!(c.heartbeat_fails, 5);
        assert_eq!(c.fetch_limit, 128);
        env::remove_var("FUSION_MEMORY_LEADER");
        env::remove_var("FUSION_MEMORY_HEARTBEAT_SECS");
        env::remove_var("FUSION_MEMORY_HEARTBEAT_FAILS");
        env::remove_var("FUSION_MEMORY_FETCH_LIMIT");
    }

    #[test]
    fn sync_config_invalid_env_falls_back_to_default() {
        // 覆盖 env 解析失败 (.parse().ok() → None → 默认) 分支。
        let _g = lock();
        env::set_var("FUSION_MEMORY_LEADER", "127.0.0.1:9");
        env::set_var("FUSION_MEMORY_HEARTBEAT_SECS", "not-a-number");
        env::set_var("FUSION_MEMORY_HEARTBEAT_FAILS", "");
        env::set_var("FUSION_MEMORY_FETCH_LIMIT", "xyz");
        let c = SyncConfig::from_env().unwrap();
        assert_eq!(c.heartbeat_secs, DEFAULT_HEARTBEAT_SECS);
        assert_eq!(c.heartbeat_fails, DEFAULT_HEARTBEAT_FAILS);
        assert_eq!(c.fetch_limit, DEFAULT_FETCH_LIMIT);
        env::remove_var("FUSION_MEMORY_LEADER");
        env::remove_var("FUSION_MEMORY_HEARTBEAT_SECS");
        env::remove_var("FUSION_MEMORY_HEARTBEAT_FAILS");
        env::remove_var("FUSION_MEMORY_FETCH_LIMIT");
    }
}
