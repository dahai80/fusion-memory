//! fm-embed 配置。PRD §12.3。

use tracing::info;

/// embedding 配置。env 覆盖见 from_env。
#[derive(Debug, Clone)]
pub struct EmbedConfig {
    /// fusion-mlx base url (OpenAI 兼容)。默认 http://127.0.0.1:11434/v1。
    pub mlx_url: String,
    /// 嵌入模型名。默认 BAAI/bge-m3 (dim=1024)。
    pub embed_model: String,
    /// mlx API key (Bearer)。
    pub api_key: String,
    /// 向量维度 (模型决定, bge-m3=1024)。
    pub dimension: usize,
    /// mlx 并发上限 (全局信号量, A3)。默认 2。
    pub mlx_concurrency: usize,
    /// query embedding LRU 缓存容量 (A3)。默认 1024。
    pub cache_capacity: usize,
    /// query embedding 缓存 TTL 秒 (A3)。默认 3600 (1h)。
    pub cache_ttl_secs: u64,
    /// HTTP 超时秒。
    pub timeout_secs: u64,
}

impl Default for EmbedConfig {
    fn default() -> Self {
        Self {
            mlx_url: "http://127.0.0.1:11434/v1".into(),
            embed_model: "BAAI/bge-m3".into(),
            api_key: String::new(),
            dimension: 1024,
            mlx_concurrency: 2,
            cache_capacity: 1024,
            cache_ttl_secs: 3600,
            timeout_secs: 30,
        }
    }
}

impl EmbedConfig {
    /// 从 env 读覆盖 (PRD §12.3)。
    pub fn from_env() -> Self {
        Self::default().overlay_env()
    }

    /// #23: 在既有配置 (默认/TOML) 上叠 env 覆盖。env 仍最高优先。
    pub fn overlay_env(mut self) -> Self {
        if let Ok(v) = std::env::var("FUSION_MLX_URL") {
            self.mlx_url = v;
        }
        if let Ok(v) = std::env::var("FUSION_MEMORY_EMBED_MODEL") {
            self.embed_model = v;
        }
        if let Ok(v) = std::env::var("FUSION_MEMORY_MLX_API_KEY") {
            self.api_key = v;
        }
        if let Ok(v) = std::env::var("FUSION_MEMORY_MLX_CONCURRENCY") {
            if let Ok(n) = v.parse::<usize>() {
                if n > 0 {
                    self.mlx_concurrency = n;
                }
            }
        }
        self
    }

    /// embeddings endpoint URL。
    pub fn embeddings_url(&self) -> String {
        format!("{}/embeddings", self.mlx_url.trim_end_matches('/'))
    }

    /// #59: api_key 为空时回落读 fusion-mlx settings.json 的 auth.api_key (单一事实来源,
    /// 免手工 key 文件漂移)。路径 env FUSION_MEMORY_MLX_SETTINGS_JSON 可覆盖, 默认
    /// ~/.fusion-mlx/settings.json。已配 key 不覆盖 (显式配置优先)。读失败静默跳过 (回落
    /// 失败不应阻塞启动; 401 时 mlx.rs 错误信息给出排查提示)。
    pub fn with_settings_fallback(mut self) -> Self {
        if self.api_key.is_empty() {
            if let Some(key) = mlx_settings_api_key() {
                info!(
                    path = %mlx_settings_path().display(),
                    "embed api_key empty, fell back to fusion-mlx settings.json auth.api_key (#59)"
                );
                self.api_key = key;
            }
        }
        self
    }
}

/// #59: fusion-mlx settings.json 路径。env FUSION_MEMORY_MLX_SETTINGS_JSON 覆盖,
/// 默认 $HOME/.fusion-mlx/settings.json。
pub fn mlx_settings_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("FUSION_MEMORY_MLX_SETTINGS_JSON") {
        return std::path::PathBuf::from(p);
    }
    if let Some(h) = std::env::var_os("HOME") {
        return std::path::PathBuf::from(h)
            .join(".fusion-mlx")
            .join("settings.json");
    }
    std::path::PathBuf::from(".fusion-mlx/settings.json")
}

/// #59: 读 fusion-mlx settings.json 的 auth.api_key。文件缺失/无 auth/api_key/空串 → None。
pub fn mlx_settings_api_key() -> Option<String> {
    let path = mlx_settings_path();
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let key = v.get("auth")?.get("api_key")?.as_str()?.trim().to_string();
    if key.is_empty() {
        None
    } else {
        Some(key)
    }
}

/// #59: 401 时附带的排查提示 (fail-visible, 免 store 层 goose chase)。
pub fn auth_rejected_hint() -> &'static str {
    "embedding key rejected by fusion-mlx — check [embed] mlx_api_key/mlx_api_key_file vs fusion-mlx settings.json auth.api_key (or unset them to use the settings.json fallback)"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // env 进程全局, 并行测试 set/remove 同名 env 串扰, 锁串行化。
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn defaults() {
        let c = EmbedConfig::default();
        assert_eq!(c.dimension, 1024);
        assert_eq!(c.mlx_concurrency, 2);
        assert_eq!(c.cache_capacity, 1024);
        assert_eq!(c.embed_model, "BAAI/bge-m3");
    }

    #[test]
    fn embeddings_url_no_double_slash() {
        let c = EmbedConfig {
            mlx_url: "http://127.0.0.1:11434/v1/".into(),
            ..Default::default()
        };
        assert_eq!(c.embeddings_url(), "http://127.0.0.1:11434/v1/embeddings");
        let c2 = EmbedConfig::default();
        assert_eq!(c2.embeddings_url(), "http://127.0.0.1:11434/v1/embeddings");
    }

    #[test]
    fn from_env_overrides_and_defaults() {
        // 单测试串行 env, 避免并行测试 env 竞争。
        let _g = ENV_LOCK.lock().unwrap();
        // 1) 全设 → 全覆盖
        std::env::set_var("FUSION_MLX_URL", "http://127.0.0.1:11432/v1");
        std::env::set_var("FUSION_MEMORY_EMBED_MODEL", "BAAI/bge-small");
        std::env::set_var("FUSION_MEMORY_MLX_API_KEY", "k-123");
        std::env::set_var("FUSION_MEMORY_MLX_CONCURRENCY", "4");
        let c = EmbedConfig::from_env();
        assert_eq!(c.mlx_url, "http://127.0.0.1:11432/v1");
        assert_eq!(c.embed_model, "BAAI/bge-small");
        assert_eq!(c.api_key, "k-123");
        assert_eq!(c.mlx_concurrency, 4);
        // 2) 非法 concurrency → 保持默认
        std::env::set_var("FUSION_MEMORY_MLX_CONCURRENCY", "not-a-num");
        assert_eq!(EmbedConfig::from_env().mlx_concurrency, 2, "非法值保持默认");
        std::env::set_var("FUSION_MEMORY_MLX_CONCURRENCY", "0");
        assert_eq!(EmbedConfig::from_env().mlx_concurrency, 2, "0 保持默认");
        // 3) 全 unset → 默认
        std::env::remove_var("FUSION_MLX_URL");
        std::env::remove_var("FUSION_MEMORY_EMBED_MODEL");
        std::env::remove_var("FUSION_MEMORY_MLX_API_KEY");
        std::env::remove_var("FUSION_MEMORY_MLX_CONCURRENCY");
        let d = EmbedConfig::from_env();
        assert_eq!(d.mlx_url, "http://127.0.0.1:11434/v1");
        assert_eq!(d.embed_model, "BAAI/bge-m3");
        assert_eq!(d.api_key, "");
        assert_eq!(d.mlx_concurrency, 2);
    }

    #[test]
    fn overlay_env_keeps_base_and_env_wins() {
        // #23: TOML/base 值保留, env 只覆盖出现的字段。
        let _g = ENV_LOCK.lock().unwrap();
        let base = EmbedConfig {
            mlx_url: "http://10.0.0.1:9000/v1".into(),
            embed_model: "custom-model".into(),
            api_key: "toml-key".into(),
            ..Default::default()
        };
        std::env::set_var("FUSION_MEMORY_MLX_API_KEY", "env-key");
        let c = base.overlay_env();
        assert_eq!(c.mlx_url, "http://10.0.0.1:9000/v1", "env 未设 → 保留 base");
        assert_eq!(c.embed_model, "custom-model");
        assert_eq!(c.api_key, "env-key", "env 设 → 覆盖 base");
        std::env::remove_var("FUSION_MEMORY_MLX_API_KEY");
    }

    // #59: settings.json 回落。临时目录 + env 路径覆盖, ENV_LOCK 串行化。
    #[test]
    fn settings_fallback_only_when_key_empty() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("fm-embed-59-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"auth": {"api_key": "settings-key-59"}}"#).unwrap();
        std::env::set_var("FUSION_MEMORY_MLX_SETTINGS_JSON", &path);

        // 空 key → 回落 settings.json
        let empty = EmbedConfig {
            api_key: String::new(),
            ..Default::default()
        };
        assert_eq!(empty.with_settings_fallback().api_key, "settings-key-59");
        // 已配 key → 不覆盖 (显式配置优先)
        let explicit = EmbedConfig {
            api_key: "explicit-key".into(),
            ..Default::default()
        };
        assert_eq!(explicit.with_settings_fallback().api_key, "explicit-key");

        std::env::remove_var("FUSION_MEMORY_MLX_SETTINGS_JSON");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_api_key_missing_or_bad_returns_none() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("fm-embed-59b-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        // 文件缺失
        std::env::set_var("FUSION_MEMORY_MLX_SETTINGS_JSON", &path);
        assert!(mlx_settings_api_key().is_none());
        // 坏 json
        std::fs::write(&path, "not-json").unwrap();
        assert!(mlx_settings_api_key().is_none());
        // 空 api_key
        std::fs::write(&path, r#"{"auth": {"api_key": ""}}"#).unwrap();
        assert!(mlx_settings_api_key().is_none());
        std::env::remove_var("FUSION_MEMORY_MLX_SETTINGS_JSON");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auth_hint_nonempty_and_actionable() {
        let h = auth_rejected_hint();
        assert!(h.contains("settings.json"));
        assert!(h.contains("mlx_api_key"));
    }
}
