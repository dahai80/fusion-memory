//! fm-server 配置。PRD §11.2/§12.3。
//!
//! HTTP 强制 Bearer 鉴权（B5）：未配 FUSION_MEMORY_API_KEY 则 HTTP 拒启。
//! UDS 走文件权限 0600 兜底（B6）。
//!
//! P1-8: TOML 配置文件 + 启动校验 + secret 文件读取（非明文 env）。
//! 配置优先级: env > TOML 文件字段 > 文件 secret_file 路径 > 默认值。
//!   env 仍可覆盖 (运维热改/调试), TOML 文件作基础配置, secret_file 读文件内容填密钥
//!   (避免密钥落 env/进程 cmdline 泄漏)。启动走 validate() 显式校验, 坏值 fail-visible 退出。

use std::path::PathBuf;

use serde::Deserialize;
use tracing::{info, warn};

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
    /// P1-5: UDS 连接级 token（可选, 多租户场景）。空 → 仅靠 sock 0600 文件权限 (向后兼容)。
    /// 配置后客户端首行须 `AUTH <token>\n` 握手, 否则后续 RPC 一律 -32004 unauthorized。
    pub uds_token: String,
    /// #16 多租户: 强制 gateway 源。true → HTTP /v1/* 须带 X-Fusion-Route: gateway-decision,
    /// 否则 403 (挡直连端口绕过 gateway 租户派生)。默认 false (单租户 dev / 直连 CLI 不破)。
    pub gateway_origin_required: bool,
    /// #16 多租户: 默认租户。无 X-Fusion-Tenant 头时用此值 (单租户部署可配固定租户名)。
    /// 空 = 默认租户 (向后兼容, 命中旧库 tenant='')。引擎级 + 请求级回退值。
    pub default_tenant: String,
}

/// P1-8: TOML 配置文件镜像。全字段可选 — 仅出现的字段覆盖默认。
/// secret_file 系列: 读文件内容 (trim) 填对应密钥, 避免密钥明文落 env/cmdline。
#[derive(Debug, Default, Deserialize)]
pub struct ConfigFile {
    pub data_dir: Option<String>,
    pub sock_path: Option<String>,
    pub http_port: Option<u16>,
    pub dim: Option<usize>,
    pub uds_enabled: Option<bool>,
    /// HTTP Bearer token 明文 (不推荐, 优先用 api_key_file)。
    pub api_key: Option<String>,
    /// P1-8: 读此文件内容作 api_key (推荐, 文件权限 0600)。
    pub api_key_file: Option<String>,
    pub uds_token: Option<String>,
    /// P1-8: 读此文件内容作 uds_token (推荐)。
    pub uds_token_file: Option<String>,
    /// #16: 强制 gateway 源 (TOML 镜像)。
    pub gateway_origin_required: Option<bool>,
    /// #16: 默认租户 (TOML 镜像)。
    pub default_tenant: Option<String>,
}

/// P1-8: 默认配置文件路径。env FM_CONFIG 显式指定, 否则 data_dir/fusion-memory.toml。
/// data_dir 本身可能由 env/file 改, 故分两段: 先定 data_dir 再拼 toml 路径。
fn config_file_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("FM_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let data_dir = default_data_dir();
    let p = data_dir.join("fusion-memory.toml");
    p.exists().then_some(p)
}

/// P1-8: 读 secret 文件内容, trim 首尾空白。文件不存在/读失败 → 返回 None + warn (fail-visible)。
fn read_secret_file(label: &str, path: &str) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let t = s.trim().to_string();
            if t.is_empty() {
                warn!(label, path, "secret file 内容为空, 忽略 (运维请检查)");
                None
            } else {
                Some(t)
            }
        }
        Err(e) => {
            warn!(label, path, error = %e, "secret file 读取失败, 忽略 (fail-visible)");
            None
        }
    }
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
            uds_token: String::new(),
            gateway_origin_required: false,
            default_tenant: String::new(),
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
    /// P1-8: 委托 apply_env (from_file_or_env 复用同逻辑)。保留公开 API 向后兼容。
    pub fn from_env() -> Self {
        let mut c = Self::default();
        c.apply_env();
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

    /// P1-8: TOML 文件 + env 合并加载。优先级 env > 文件字段 > 文件 secret_file > 默认。
    /// 流程: 读文件 (若存在) → 覆盖默认 → env 覆盖 → secret_file 读填 (仅当字段仍空)。
    /// secret_file 不被 env 覆盖语义干扰: 它是"密钥来源", env 未设密钥时才从文件读。
    pub fn from_file_or_env() -> Self {
        let mut c = Self::default();
        if let Some(path) = config_file_path() {
            match std::fs::read_to_string(&path) {
                Ok(text) => match toml::from_str::<ConfigFile>(&text) {
                    Ok(f) => {
                        info!(path = ?path, "loaded TOML config");
                        c.apply_file(&f);
                    }
                    Err(e) => {
                        warn!(path = ?path, error = %e, "TOML 解析失败, 回退默认+env (fail-visible)");
                    }
                },
                Err(e) => {
                    warn!(path = ?path, error = %e, "配置文件读取失败, 回退默认+env");
                }
            }
        }
        // env 覆盖 (运维热改优先)
        c.apply_env();
        // secret_file 读填: 仅当对应密钥仍空 (env/文件字段都没给) 时, 从文件读。
        // 注意: secret_file 路径本身可能来自文件或 env, 故 apply_env 后再统一读填。
        c.fill_secrets_from_files();
        c
    }

    /// P1-8: 文件字段覆盖默认 (仅出现的字段)。
    fn apply_file(&mut self, f: &ConfigFile) {
        if let Some(d) = &f.data_dir {
            self.data_dir = PathBuf::from(d);
            self.sock_path = self.data_dir.join("fusion-memory.sock");
        }
        if let Some(s) = &f.sock_path {
            self.sock_path = PathBuf::from(s);
        }
        if let Some(p) = f.http_port {
            self.http_port = p;
        }
        if let Some(d) = f.dim {
            self.dim = d;
        }
        if let Some(u) = f.uds_enabled {
            self.uds_enabled = u;
        }
        if let Some(k) = &f.api_key {
            self.api_key = k.clone();
        }
        if let Some(t) = &f.uds_token {
            self.uds_token = t.clone();
        }
        if let Some(g) = f.gateway_origin_required {
            self.gateway_origin_required = g;
        }
        if let Some(t) = &f.default_tenant {
            self.default_tenant = t.clone();
        }
        // secret_file 路径暂存到临时字段? 不 — ConfigFile 不持有运行态。
        // 改: 在 fill_secrets_from_files 时无法再拿到路径 (ServerConfig 无 path 字段)。
        // 故此处直接读 secret_file 填密钥 (文件字段优先级 < env, env 后续覆盖)。
        if let Some(p) = &f.api_key_file {
            if self.api_key.is_empty() {
                if let Some(v) = read_secret_file("api_key", p) {
                    self.api_key = v;
                }
            }
        }
        if let Some(p) = &f.uds_token_file {
            if self.uds_token.is_empty() {
                if let Some(v) = read_secret_file("uds_token", p) {
                    self.uds_token = v;
                }
            }
        }
        // 记录 secret_file 路径供 env 阶段后重读? 不需要 — env 若给了密钥直接覆盖, 无需再读文件。
        // 但若 env 没给密钥、文件给了 secret_file 路径, 上面已读填。env 阶段不会再设空覆盖。
        // 唯一缺口: env 设了空字符串? env::var 设空串仍是 Ok, 会覆盖为空。罕见, 接受。
    }

    /// P1-8: env 覆盖 (从 from_env 抽出, 复用)。
    fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("FM_HOME") {
            self.data_dir = PathBuf::from(v);
            self.sock_path = self.data_dir.join("fusion-memory.sock");
        }
        if let Ok(v) = std::env::var("FUSION_MEMORY_SOCK") {
            self.sock_path = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("FUSION_MEMORY_HTTP_PORT") {
            match v.parse::<u16>() {
                Ok(p) => self.http_port = p,
                Err(e) => warn!(
                    raw = %v,
                    error = %e,
                    fallback = self.http_port,
                    "FUSION_MEMORY_HTTP_PORT 坏值, 回退 (运维请检查 env 拼写)"
                ),
            }
        }
        if let Ok(v) = std::env::var("FUSION_MEMORY_API_KEY") {
            self.api_key = v;
        }
        if let Ok(v) = std::env::var("FUSION_MEMORY_UDS_TOKEN") {
            self.uds_token = v;
        }
        if let Ok(v) = std::env::var("FUSION_MEMORY_GATEWAY_ORIGIN_REQUIRED") {
            match v.parse::<bool>() {
                Ok(b) => self.gateway_origin_required = b,
                Err(e) => warn!(
                    raw = %v,
                    error = %e,
                    "FUSION_MEMORY_GATEWAY_ORIGIN_REQUIRED 坏值 (期望 true/false), 回退默认"
                ),
            }
        }
        if let Ok(v) = std::env::var("FUSION_MEMORY_DEFAULT_TENANT") {
            self.default_tenant = v;
        }
        if let Ok(v) = std::env::var("FUSION_MEMORY_DIM") {
            match v.parse::<usize>() {
                Ok(d) if d > 0 => self.dim = d,
                Ok(d) => warn!(
                    raw = %v,
                    dim = d,
                    fallback = self.dim,
                    "FUSION_MEMORY_DIM=0 非法, 回退 (嵌入维度须 >0)"
                ),
                Err(e) => warn!(
                    raw = %v,
                    error = %e,
                    fallback = self.dim,
                    "FUSION_MEMORY_DIM 坏值, 回退 (运维请检查 env 拼写)"
                ),
            }
        }
    }

    /// P1-8: env 阶段后, 若密钥仍空且 env 给了 secret_file 路径, 读填。
    /// env secret_file 路径变量: FUSION_MEMORY_API_KEY_FILE / FUSION_MEMORY_UDS_TOKEN_FILE。
    fn fill_secrets_from_files(&mut self) {
        if self.api_key.is_empty() {
            if let Ok(p) = std::env::var("FUSION_MEMORY_API_KEY_FILE") {
                if let Some(v) = read_secret_file("api_key", &p) {
                    self.api_key = v;
                }
            }
        }
        if self.uds_token.is_empty() {
            if let Ok(p) = std::env::var("FUSION_MEMORY_UDS_TOKEN_FILE") {
                if let Some(v) = read_secret_file("uds_token", &p) {
                    self.uds_token = v;
                }
            }
        }
    }

    /// P1-8: 启动校验。坏配置 fail-visible 返回 Err, main 据此 exit(1) 不裸跑。
    /// 校验: dim>0, http_port 不超 65535 (u16 天然), uds_enabled 时 sock_path 非空,
    /// api_key_file/uds_token_file 若配则文件可读 (已在 read 时 warn, 此处不重复)。
    pub fn validate(&self) -> Result<(), String> {
        if self.dim == 0 {
            return Err(format!(
                "dim=0 非法 (嵌入维度须 >0), config dim={}",
                self.dim
            ));
        }
        if self.uds_enabled && self.sock_path.as_os_str().is_empty() {
            return Err("uds_enabled=true 但 sock_path 空".into());
        }
        if self.http_port > 0 && self.api_key.is_empty() {
            // http_needs_token — 不当致命错误 (serve 会拒启 HTTP 仅 UDS), 仅 warn 已够。
            // 但 audit P1-8 要"启动校验", 这里返 Err 让运维明确知道 HTTP 不会起。
            return Err(format!(
                "http_port={} 开但 api_key 未配 (B5 拒启 HTTP)。配 FUSION_MEMORY_API_KEY 或 api_key_file, 或关 HTTP (http_port=0)",
                self.http_port
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // P1-8: env 是进程全局, 多测试并发 set/remove 同名 env 会串扰。
    // 锁串行所有改 env 的测试 (含旧 from_env 测试)。
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn defaults() {
        let c = ServerConfig::default();
        assert_eq!(c.http_port, 11435);
        assert_eq!(c.dim, 1024);
        assert!(c.uds_enabled);
        assert!(c.api_key.is_empty());
        assert!(c.sock_path.ends_with("fusion-memory.sock"));
        assert!(c.uds_token.is_empty());
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
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("FM_HOME", "/tmp/fm-cfg-env-home");
        std::env::set_var("FUSION_MEMORY_API_KEY", "envkey");
        std::env::set_var("FUSION_MEMORY_HTTP_PORT", "9999");
        std::env::set_var("FUSION_MEMORY_DIM", "512");
        std::env::set_var("FUSION_MEMORY_UDS_TOKEN", "uds-tok");
        let c = ServerConfig::from_env();
        assert_eq!(c.data_dir, std::path::PathBuf::from("/tmp/fm-cfg-env-home"));
        assert_eq!(c.api_key, "envkey");
        assert_eq!(c.http_port, 9999);
        assert_eq!(c.dim, 512);
        assert_eq!(c.uds_token, "uds-tok");
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
        std::env::remove_var("FUSION_MEMORY_UDS_TOKEN");
    }

    // ---- P1-8: TOML 配置文件 + secret 文件 + validate ----
    // env 测试并发串扰, 每个测试自洽设/清 FM_CONFIG + 密钥 env, 不依赖外部文件。

    #[test]
    fn p1_8_file_overrides_defaults() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("fm.toml");
        std::fs::write(
            &toml_path,
            "http_port = 8888\ndim = 768\nuds_enabled = false\napi_key = \"filekey\"\n",
        )
        .unwrap();
        std::env::set_var("FM_CONFIG", &toml_path);
        // 清密钥 env 防 bleed
        std::env::remove_var("FUSION_MEMORY_API_KEY");
        std::env::remove_var("FUSION_MEMORY_HTTP_PORT");
        std::env::remove_var("FUSION_MEMORY_DIM");
        let c = ServerConfig::from_file_or_env();
        assert_eq!(c.http_port, 8888);
        assert_eq!(c.dim, 768);
        assert!(!c.uds_enabled);
        assert_eq!(c.api_key, "filekey");
        std::env::remove_var("FM_CONFIG");
    }

    #[test]
    fn p1_8_env_overrides_file() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("fm.toml");
        std::fs::write(&toml_path, "http_port = 8888\napi_key = \"filekey\"\n").unwrap();
        std::env::set_var("FM_CONFIG", &toml_path);
        std::env::set_var("FUSION_MEMORY_HTTP_PORT", "7777");
        std::env::set_var("FUSION_MEMORY_API_KEY", "envkey");
        let c = ServerConfig::from_file_or_env();
        // env 赢
        assert_eq!(c.http_port, 7777);
        assert_eq!(c.api_key, "envkey");
        std::env::remove_var("FM_CONFIG");
        std::env::remove_var("FUSION_MEMORY_HTTP_PORT");
        std::env::remove_var("FUSION_MEMORY_API_KEY");
    }

    #[test]
    fn p1_8_secret_file_read() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("api.key");
        std::fs::write(&key_file, "  secret-from-file  \n").unwrap();
        let toml_path = dir.path().join("fm.toml");
        std::fs::write(
            &toml_path,
            format!(
                "http_port = 9000\napi_key_file = \"{}\"\n",
                key_file.display()
            ),
        )
        .unwrap();
        std::env::set_var("FM_CONFIG", &toml_path);
        std::env::remove_var("FUSION_MEMORY_API_KEY");
        std::env::remove_var("FUSION_MEMORY_API_KEY_FILE");
        let c = ServerConfig::from_file_or_env();
        // trim 首尾空白
        assert_eq!(c.api_key, "secret-from-file");
        std::env::remove_var("FM_CONFIG");
    }

    #[test]
    fn p1_8_secret_file_env_path() {
        // env 给 secret_file 路径 (非明文密钥)
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("uds.key");
        std::fs::write(&key_file, "uds-secret\n").unwrap();
        // 无 TOML 文件: FM_CONFIG 指向不存在路径 → config_file_path 返回该路径但读取失败 warn 回退
        std::env::set_var("FM_CONFIG", dir.path().join("nope.toml"));
        std::env::set_var("FUSION_MEMORY_UDS_TOKEN_FILE", &key_file);
        std::env::remove_var("FUSION_MEMORY_UDS_TOKEN");
        let c = ServerConfig::from_file_or_env();
        assert_eq!(c.uds_token, "uds-secret");
        std::env::remove_var("FM_CONFIG");
        std::env::remove_var("FUSION_MEMORY_UDS_TOKEN_FILE");
    }

    #[test]
    fn p1_8_validate_rejects_zero_dim() {
        let c = ServerConfig {
            dim: 0,
            http_port: 0, // 关 HTTP 避免 api_key 校验
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn p1_8_validate_rejects_http_without_key() {
        let c = ServerConfig {
            http_port: 11435,
            api_key: String::new(),
            ..Default::default()
        };
        let e = c.validate().unwrap_err();
        assert!(e.contains("api_key"), "err={e}");
    }

    #[test]
    fn p1_8_validate_passes_good_config() {
        let c = ServerConfig {
            http_port: 0, // 仅 UDS, 无需 api_key
            ..Default::default()
        };
        assert!(c.validate().is_ok());
        let c2 = ServerConfig {
            http_port: 11435,
            api_key: "k".into(),
            ..Default::default()
        };
        assert!(c2.validate().is_ok());
    }

    #[test]
    fn p1_8_bad_toml_falls_back_to_env() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("fm.toml");
        std::fs::write(&toml_path, "this is = = not valid toml {{{").unwrap();
        std::env::set_var("FM_CONFIG", &toml_path);
        std::env::set_var("FUSION_MEMORY_DIM", "512");
        std::env::remove_var("FUSION_MEMORY_API_KEY");
        std::env::remove_var("FUSION_MEMORY_HTTP_PORT");
        let c = ServerConfig::from_file_or_env();
        // 坏 TOML 回退, env 仍生效
        assert_eq!(c.dim, 512);
        assert_eq!(c.http_port, 11435); // 默认
        std::env::remove_var("FM_CONFIG");
        std::env::remove_var("FUSION_MEMORY_DIM");
    }
}
