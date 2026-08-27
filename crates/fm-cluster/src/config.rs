use std::env;

pub const DEFAULT_SYNC_PORT: u16 = 11436;
pub const DEFAULT_HEARTBEAT_SECS: u64 = 5;
pub const DEFAULT_HEARTBEAT_FAILS: u32 = 3;
pub const DEFAULT_FETCH_LIMIT: usize = 256;

/// 集群全局配置 (角色 + 端口)。
#[derive(Debug, Clone)]
pub struct ClusterConfig {
    pub sync_port: u16,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            sync_port: env::var("FUSION_MEMORY_SYNC_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_SYNC_PORT),
        }
    }
}

/// follower 拉取/心跳配置。PRD §16: 心跳 5s, 3 失败 = leader down。
#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub leader_addr: String,
    pub heartbeat_secs: u64,
    pub heartbeat_fails: u32,
    pub fetch_limit: usize,
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
        assert_eq!(ClusterConfig::default().sync_port, DEFAULT_SYNC_PORT);
    }

    #[test]
    fn cluster_config_env_override() {
        let _g = lock();
        env::set_var("FUSION_MEMORY_SYNC_PORT", "12000");
        assert_eq!(ClusterConfig::default().sync_port, 12000);
        env::remove_var("FUSION_MEMORY_SYNC_PORT");
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
