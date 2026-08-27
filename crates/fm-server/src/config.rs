//! fm-server 配置。PRD §11.2/§12.3。
//!
//! HTTP 强制 Bearer 鉴权（B5）：未配 FUSION_MEMORY_API_KEY 则 HTTP 拒启。
//! UDS 走文件权限 0600 兜底（B6）。

use std::path::PathBuf;

use tracing::warn;

/// 服务配置。
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// 数据目录（~/.fusion-memory）。
    pub data_dir: PathBuf,
    /// UDS sock 路径。
    pub sock_path: PathBuf,
    /// HTTP 端口（0 = 关闭）。
    pub http_port: u16,
    /// HTTP Bearer token（必配，空则 HTTP 拒启）。
    pub api_key: String,
    /// 嵌入维度（bge-m3=1024）。
    pub dim: usize,
    /// 是否启用 UDS。
    pub uds_enabled: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        let data_dir = default_data_dir();
        let sock_path = data_dir.join("fusion-memory.sock");
        Self {
            data_dir,
            sock_path,
            http_port: 11435,
            api_key: String::new(),
            dim: 1024,
            uds_enabled: true,
        }
    }
}

fn default_data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("FM_HOME") {
        return PathBuf::from(d);
    }
    if let Some(home) = dirs_home() {
        return home.join(".fusion-memory");
    }
    PathBuf::from(".fusion-memory")
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

impl ServerConfig {
    /// 从 env 读覆盖。PRD §12.3。§2.10: 坏值不再静默回退默认, 改 warn 结构化日志 (fail-visible)。
    /// 不 hard-fail: 运行中服务因端口笔误 panic 更糟; warn 让运维从日志诊断哪个 env 坏。
    pub fn from_env() -> Self {
        let mut c = Self::default();
        if let Ok(v) = std::env::var("FM_HOME") {
            c.data_dir = PathBuf::from(v);
            c.sock_path = c.data_dir.join("fusion-memory.sock");
        }
        if let Ok(v) = std::env::var("FUSION_MEMORY_SOCK") {
            c.sock_path = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("FUSION_MEMORY_HTTP_PORT") {
            match v.parse::<u16>() {
                Ok(p) => c.http_port = p,
                Err(e) => warn!(
                    raw = %v,
                    error = %e,
                    fallback = c.http_port,
                    "FUSION_MEMORY_HTTP_PORT 坏值, 回退默认 (运维请检查 env 拼写)"
                ),
            }
        }
        if let Ok(v) = std::env::var("FUSION_MEMORY_API_KEY") {
            c.api_key = v;
        }
        if let Ok(v) = std::env::var("FUSION_MEMORY_DIM") {
            match v.parse::<usize>() {
                Ok(d) if d > 0 => c.dim = d,
                Ok(d) => warn!(
                    raw = %v,
                    dim = d,
                    fallback = c.dim,
                    "FUSION_MEMORY_DIM=0 非法, 回退默认 (嵌入维度须 >0)"
                ),
                Err(e) => warn!(
                    raw = %v,
                    error = %e,
                    fallback = c.dim,
                    "FUSION_MEMORY_DIM 坏值, 回退默认 (运维请检查 env 拼写)"
                ),
            }
        }
        c
    }

    /// HTTP 是否可启（端口开 + token 已配）。B5: 未配 token 拒启。
    pub fn http_ok(&self) -> bool {
        self.http_port > 0 && !self.api_key.is_empty()
    }

    /// HTTP 未配 token 但端口开 → 启动方需显式拒绝（调用方判断）。
    pub fn http_needs_token(&self) -> bool {
        self.http_port > 0 && self.api_key.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = ServerConfig::default();
        assert_eq!(c.http_port, 11435);
        assert_eq!(c.dim, 1024);
        assert!(c.uds_enabled);
        assert!(c.api_key.is_empty());
        assert!(c.sock_path.ends_with("fusion-memory.sock"));
    }

    #[test]
    fn http_gate() {
        let c = ServerConfig {
            api_key: "k".into(),
            ..Default::default()
        };
        assert!(c.http_ok());
        assert!(!c.http_needs_token());
        let c2 = ServerConfig::default();
        assert!(!c2.http_ok());
        assert!(c2.http_needs_token());
        let c3 = ServerConfig {
            api_key: "k".into(),
            http_port: 0,
            ..Default::default()
        };
        assert!(!c3.http_ok());
    }

    #[test]
    fn from_env_overrides_and_bad_values_warn_and_fallback() {
        // 单测合并：env 测试并发跑会串扰，合并成顺序块，自洽设/清。
        // §2.10: 坏值不再静默回退, 改 warn (fail-visible)。行为仍是回退默认值 (不 panic),
        // 但测试名+注释显式标注这是 fail-visible 路径, 非"坏值忽略"。
        std::env::set_var("FM_HOME", "/tmp/fm-cfg-env-home");
        std::env::set_var("FUSION_MEMORY_API_KEY", "envkey");
        std::env::set_var("FUSION_MEMORY_HTTP_PORT", "9999");
        std::env::set_var("FUSION_MEMORY_DIM", "512");
        let c = ServerConfig::from_env();
        assert_eq!(c.data_dir, std::path::PathBuf::from("/tmp/fm-cfg-env-home"));
        assert_eq!(c.api_key, "envkey");
        assert_eq!(c.http_port, 9999);
        assert_eq!(c.dim, 512);
        assert!(c.http_ok());

        // 坏值：端口非数字、dim=0 → 回退默认 (同时发 warn, 见 from_env 实现)
        std::env::set_var("FUSION_MEMORY_HTTP_PORT", "not-a-port");
        std::env::set_var("FUSION_MEMORY_DIM", "0");
        let c2 = ServerConfig::from_env();
        assert_eq!(c2.http_port, 11435);
        assert_eq!(c2.dim, 1024);

        std::env::remove_var("FM_HOME");
        std::env::remove_var("FUSION_MEMORY_API_KEY");
        std::env::remove_var("FUSION_MEMORY_HTTP_PORT");
        std::env::remove_var("FUSION_MEMORY_DIM");
    }
}
