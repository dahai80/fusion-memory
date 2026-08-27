use std::env;
use std::fmt;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    Standalone,
    Leader,
    Follower,
}

impl NodeRole {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeRole::Standalone => "standalone",
            NodeRole::Leader => "leader",
            NodeRole::Follower => "follower",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "leader" => NodeRole::Leader,
            "follower" => NodeRole::Follower,
            _ => NodeRole::Standalone,
        }
    }
}

impl fmt::Display for NodeRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 从 FUSION_MEMORY_ROLE 解析角色。缺失/非法 → standalone。PRD §16。
pub fn detect_role() -> NodeRole {
    match env::var("FUSION_MEMORY_ROLE") {
        Ok(v) => NodeRole::parse(&v),
        Err(_) => NodeRole::Standalone,
    }
}

/// 角色解析 (env 优先, 次读 home/role 文件, 末 standalone)。PRD §16 手动 failover。
/// env 优先使临时调试不改文件; 文件持久化供 fm cluster promote 落地 + 重启生效。
pub fn detect_role_with_home(home: Option<&Path>) -> NodeRole {
    if let Ok(v) = env::var("FUSION_MEMORY_ROLE") {
        return NodeRole::parse(&v);
    }
    if let Some(h) = home {
        let role_file = h.join("role");
        if let Ok(s) = std::fs::read_to_string(&role_file) {
            let r = NodeRole::parse(s.trim());
            tracing::debug!(%r, "role from file");
            return r;
        }
    }
    NodeRole::Standalone
}

/// 把角色写入 home/role 文件 (手动 failover 落地)。返回写入路径。
pub fn write_role_file(home: &Path, role: NodeRole) -> Result<std::path::PathBuf, std::io::Error> {
    std::fs::create_dir_all(home)?;
    let p = home.join("role");
    std::fs::write(&p, role.as_str())?;
    tracing::info!(path = %p.display(), %role, "role file written");
    Ok(p)
}

/// §1.8: 读 home/epoch 文件 (手动 failover 递增的 leader epoch)。缺失/损坏 → 0 (不 fencing)。
/// env FUSION_MEMORY_CLUSTER_EPOCH 优先 (见 ClusterConfig::with_home)。
pub fn read_epoch_file(home: &Path) -> u64 {
    match std::fs::read_to_string(home.join("epoch")) {
        Ok(s) => s.trim().parse().unwrap_or_else(|e| {
            tracing::warn!(raw = %s.trim(), error = %e, "epoch file corrupt, fall back to 0");
            0
        }),
        Err(_) => 0,
    }
}

/// §1.8: 写 epoch 到 home/epoch 文件 (fm cluster promote 落地)。返回写入路径。
pub fn write_epoch_file(home: &Path, epoch: u64) -> Result<std::path::PathBuf, std::io::Error> {
    std::fs::create_dir_all(home)?;
    let p = home.join("epoch");
    std::fs::write(&p, epoch.to_string())?;
    tracing::info!(path = %p.display(), epoch, "epoch file written");
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    #[test]
    fn parse_variants() {
        assert_eq!(NodeRole::parse("leader"), NodeRole::Leader);
        assert_eq!(NodeRole::parse("FOLLOWER"), NodeRole::Follower);
        assert_eq!(NodeRole::parse("standalone"), NodeRole::Standalone);
        assert_eq!(NodeRole::parse("garbage"), NodeRole::Standalone);
    }

    #[test]
    fn detect_env_missing_is_standalone() {
        let _g = lock();
        env::remove_var("FUSION_MEMORY_ROLE");
        assert_eq!(detect_role(), NodeRole::Standalone);
    }

    #[test]
    fn detect_env_leader() {
        let _g = lock();
        env::set_var("FUSION_MEMORY_ROLE", "leader");
        assert_eq!(detect_role(), NodeRole::Leader);
        env::remove_var("FUSION_MEMORY_ROLE");
    }

    #[test]
    fn detect_with_home_file_follower() {
        let _g = lock();
        env::remove_var("FUSION_MEMORY_ROLE");
        let dir = tempfile::tempdir().unwrap();
        let p = write_role_file(dir.path(), NodeRole::Follower).unwrap();
        assert!(p.ends_with("role"));
        assert_eq!(detect_role_with_home(Some(dir.path())), NodeRole::Follower);
    }

    #[test]
    fn detect_env_overrides_file() {
        let _g = lock();
        let dir = tempfile::tempdir().unwrap();
        write_role_file(dir.path(), NodeRole::Follower).unwrap();
        env::set_var("FUSION_MEMORY_ROLE", "leader");
        // env 优先于文件
        assert_eq!(detect_role_with_home(Some(dir.path())), NodeRole::Leader);
        env::remove_var("FUSION_MEMORY_ROLE");
    }

    #[test]
    fn detect_no_env_no_file_standalone() {
        let _g = lock();
        env::remove_var("FUSION_MEMORY_ROLE");
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            detect_role_with_home(Some(dir.path())),
            NodeRole::Standalone
        );
        assert_eq!(detect_role_with_home(None), NodeRole::Standalone);
    }
}
